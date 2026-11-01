//! Actual profile/instance journal tests. Candidate fields here are quota-only
//! fixtures: none constructs a completed save or destination outcome authority.
use super::*;
use crate::{
    file_safety::NativeLease,
    large_document::{root::PersistentRootContext, Limits as StoreLimits},
};
use std::path::PathBuf;

struct Fixture {
    base: PathBuf,
    held: Option<Directory>,
}
impl Fixture {
    fn new() -> Self {
        let base = std::env::temp_dir().join(format!(
            "peerbrush-external-intent-{}",
            uuid::Uuid::new_v4()
        ));
        fs::create_dir(&base).unwrap();
        let base = fs::canonicalize(base).unwrap();
        Self {
            held: Some(Directory::open(&base).unwrap()),
            base,
        }
    }
    fn connection(&self) -> (crate::server::Shared, crate::server::Connection) {
        let engine = Arc::new(std::sync::Mutex::new(crate::engine::Engine::new()));
        let connection = crate::server::start(engine.clone(), self.base.join("runtime")).unwrap();
        (engine, connection)
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let parent = fs::canonicalize(std::env::temp_dir()).unwrap();
        if self.base.starts_with(parent)
            && self.base.file_name().is_some_and(|n| {
                n.to_string_lossy()
                    .starts_with("peerbrush-external-intent-")
            })
            && self.held.as_ref().is_some_and(|h| h.check().is_ok())
            && fs::canonicalize(&self.base).is_ok_and(|p| p == self.base)
        {
            drop(self.held.take());
            let _ = fs::remove_dir_all(&self.base);
        }
    }
}
fn limits() -> super::super::Limits {
    super::super::Limits {
        resident_bytes: 64 * 1024 * 1024,
        disk_bytes: 16 * 1024 * 1024,
        max_files: 32,
        store: StoreLimits {
            disk_bytes: 4 * 1024 * 1024,
            max_blocks: 64,
            max_resident_blocks: 16,
            manifest_bytes: 1024 * 1024,
        },
    }
}
fn quota_frame(
    session: &SourceSession,
    observed: &NativeExternalSnapshot<'_>,
) -> Admitted<IntentFrame> {
    let owner = session
        .runtime
        .inventory
        .lock()
        .unwrap()
        .as_ref()
        .unwrap()
        .owner;
    let charge = MetadataCharge::reserve(&session.resident(), 1024 * 1024, &|| Ok(())).unwrap();
    let id = *uuid::Uuid::new_v4().as_bytes();
    let uuid = uuid::Uuid::from_bytes(id);
    let expected = observed.expected().map(|v| Expected {
        identity: v.identity,
        bytes: v.bytes,
        modified: v.modified_nanos(),
        sha256: v.sha256(),
    });
    let mut facts = Vec::new();
    if let Some(f) = observed.source() {
        facts.push(NamedFact {
            role: Role::Target,
            name: observed.target().file_name().unwrap().to_owned(),
            fact: convert(f),
        });
    }
    for (name, f) in observed.backups() {
        facts.push(NamedFact {
            role: Role::Backup,
            name: name.to_owned(),
            fact: convert(f),
        });
    }
    facts.sort_unstable_by_key(|f| f.fact.identity);
    let frame = IntentFrame {
        id,
        owner,
        operation: session.id,
        target: observed.target().to_owned(),
        target_key: [88; 32],
        previous: None,
        parent: observed.destination_parent_identity(),
        backups: observed.backup_directory().to_owned(),
        backups_identity: observed.backup_directory_identity(),
        expected,
        candidate_bytes: 7,
        candidate_sha256: digest(&SHA256, b"newdata").as_ref().try_into().unwrap(),
        provenance: [4; 32],
        core: [5; 32],
        render: [6; 32],
        temporary: format!(".peerbrush-native-{uuid}.tmp").into(),
        backup: expected.map(|_| format!("prior-{uuid}.psb").into()),
        growth: 7 + expected.map_or(0, |e| e.bytes),
        facts,
    };
    frame.validate().unwrap();
    Admitted::from_parts(Arc::new(frame), charge)
}
#[test]
fn durable_pending_drop_and_restart_rebuild_exact_debt_without_resuming_a_writer() {
    let f = Fixture::new();
    let (_engine, connection) = f.connection();
    let context = PersistentRootContext::from_explicit_state(&connection).unwrap();
    let runtime = SourceRuntime::new(context.clone(), limits()).unwrap();
    let session = runtime.begin().unwrap();
    let path = f.base.join("external.psb");
    fs::write(&path, b"old").unwrap();
    let lease = NativeLease::acquire(&path, || Ok(())).unwrap();
    let inventory = lease
        .freeze_inventory(&session.resident(), &|| Ok(()))
        .unwrap();
    let observed = inventory.observe_external(&|| Ok(())).unwrap();
    let frame = quota_frame(&session, &observed);
    let id = frame.id;
    let intent = session
        .persist_pending(frame, &observed, &|| Ok(()))
        .unwrap();
    intent.check().unwrap();
    let plan = intent.plan(&observed, &|| Ok(())).unwrap();
    plan.check_session(&session).unwrap();
    let foreign = runtime.begin().unwrap();
    assert!(plan.check_session(&foreign).is_err());
    drop(foreign);
    assert!(plan.uses_budgets(&session.resident(), &session.disk()));
    assert_eq!(plan.intent_sha256(), intent.sha256());
    let locked = runtime.inventory.lock().unwrap();
    assert!(intent.check_record().is_err());
    assert!(!runtime.tainted.load(Ordering::Acquire));
    drop(locked);
    assert!(!path
        .parent()
        .unwrap()
        .join(intent.temporary_name())
        .exists());
    assert!(!observed
        .backup_directory()
        .join(intent.backup_name().unwrap())
        .exists());
    assert_eq!(fs::read(&path).unwrap(), b"old");
    let bytes = runtime.disk().admitted().unwrap();
    let file_bytes = runtime.disk().used().unwrap();
    assert_eq!(bytes, file_bytes + 3 + 10);
    assert!(file_bytes > 56);
    // A second publication is refused before another journal file can appear.
    let next = quota_frame(&session, &observed);
    assert!(session
        .persist_pending(next, &observed, &|| Ok(()))
        .is_err());
    assert_eq!(runtime.disk().admitted().unwrap(), bytes);
    drop(intent);
    drop(plan);
    assert_eq!(runtime.disk().admitted().unwrap(), bytes);
    drop(observed);
    drop(inventory);
    drop(lease);
    drop(session);
    drop(runtime);
    let restored = SourceRuntime::new(context, limits()).unwrap();
    let session = restored.begin().unwrap();
    assert_eq!(restored.disk().admitted().unwrap(), bytes);
    let state = restored.inventory.lock().unwrap();
    let entries = &state.as_ref().unwrap().publications.entries;
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].frame.id, id);
    assert!(entries[0].file.is_some());
    drop(state);
    assert_eq!(fs::read(path).unwrap(), b"old");
    drop(session);
    drop(restored);
    drop(connection);
}
#[test]
fn partial_intent_latches_existing_runtime_and_restart_refuses_without_deleting_bytes() {
    let f = Fixture::new();
    let (_engine, connection) = f.connection();
    let context = PersistentRootContext::from_explicit_state(&connection).unwrap();
    let runtime = SourceRuntime::new(context.clone(), limits()).unwrap();
    let session = runtime.begin().unwrap();
    let path = f.base.join("external.psb");
    fs::write(&path, b"old").unwrap();
    let lease = NativeLease::acquire(&path, || Ok(())).unwrap();
    let inventory = lease
        .freeze_inventory(&session.resident(), &|| Ok(()))
        .unwrap();
    let observed = inventory.observe_external(&|| Ok(())).unwrap();
    let frame = quota_frame(&session, &observed);
    let pending = session.directory.join(format!(
        "external-pending-{}",
        uuid::Uuid::from_bytes(frame.id)
    ));
    let reached = std::cell::Cell::new(false);
    assert!(session
        .persist_pending(frame, &observed, &|| {
            if fs::symlink_metadata(&pending)
                .is_ok_and(|m| m.is_file() && !m.file_type().is_symlink() && m.len() != 0)
            {
                reached.set(true);
                Err("injected after actual write".into())
            } else {
                Ok(())
            }
        })
        .is_err());
    assert!(reached.get());
    assert!(fs::metadata(&pending).unwrap().len() > 0);
    assert!(session.check().is_err());
    assert_eq!(fs::read(&path).unwrap(), b"old");
    drop(observed);
    drop(inventory);
    drop(lease);
    drop(session);
    drop(runtime);
    let restored = SourceRuntime::new(context, limits()).unwrap();
    assert!(restored.begin().is_err());
    assert!(fs::metadata(pending).unwrap().len() > 0);
    assert_eq!(fs::read(path).unwrap(), b"old");
    drop(restored);
    drop(connection);
}
#[test]
fn same_bytes_foreign_journal_replacement_refuses_and_preserves_both_names() {
    let f = Fixture::new();
    let (_engine, connection) = f.connection();
    let context = PersistentRootContext::from_explicit_state(&connection).unwrap();
    let runtime = SourceRuntime::new(context, limits()).unwrap();
    let session = runtime.begin().unwrap();
    let path = f.base.join("external.psb");
    fs::write(&path, b"old").unwrap();
    let lease = NativeLease::acquire(&path, || Ok(())).unwrap();
    let inventory = lease
        .freeze_inventory(&session.resident(), &|| Ok(()))
        .unwrap();
    let observed = inventory.observe_external(&|| Ok(())).unwrap();
    let frame = quota_frame(&session, &observed);
    let intent = session
        .persist_pending(frame, &observed, &|| Ok(()))
        .unwrap();
    let name = session.directory.join(format!(
        "external-intent-{}.pbi",
        uuid::Uuid::from_bytes(intent.id())
    ));
    let bytes = fs::read(&name).unwrap();
    let old = session.directory.join("foreign-held-intent.pbi");
    fs::rename(&name, &old).unwrap();
    fs::write(&name, &bytes).unwrap();
    let debt = runtime.disk().admitted().unwrap();
    assert!(intent.check().is_err());
    assert!(session.check().is_err());
    assert_eq!(runtime.disk().admitted().unwrap(), debt);
    assert_eq!(fs::read(&name).unwrap(), bytes);
    assert_eq!(fs::read(&old).unwrap(), bytes);
    assert_eq!(fs::read(path).unwrap(), b"old");
    drop(intent);
    drop(observed);
    drop(inventory);
    drop(lease);
    drop(session);
    drop(runtime);
    drop(connection);
}

#[test]
fn same_inode_journal_corruption_with_restored_stamp_latches_complete_sha_failure() {
    let f = Fixture::new();
    let (_engine, connection) = f.connection();
    let context = PersistentRootContext::from_explicit_state(&connection).unwrap();
    let runtime = SourceRuntime::new(context, limits()).unwrap();
    let session = runtime.begin().unwrap();
    let path = f.base.join("external.psb");
    fs::write(&path, b"old").unwrap();
    let lease = NativeLease::acquire(&path, || Ok(())).unwrap();
    let inventory = lease
        .freeze_inventory(&session.resident(), &|| Ok(()))
        .unwrap();
    let observed = inventory.observe_external(&|| Ok(())).unwrap();
    let intent = session
        .persist_pending(quota_frame(&session, &observed), &observed, &|| Ok(()))
        .unwrap();
    let state = runtime.inventory.lock().unwrap();
    let inventory_ref = state.as_ref().unwrap();
    let entry = inventory_ref
        .publications
        .entries
        .iter()
        .find(|r| r.sha256 == intent.sha256())
        .unwrap();
    let held = inventory_ref
        .files
        .iter()
        .find(|h| Some(h.identity) == entry.file)
        .unwrap();
    let mut file = &held.file;
    file.seek(SeekFrom::Start(0)).unwrap();
    file.write_all(b"X").unwrap();
    file.sync_all().unwrap();
    held.file
        .set_times(std::fs::FileTimes::new().set_modified(held.modified))
        .unwrap();
    held.check().unwrap();
    drop(state);
    assert!(intent.check().is_err());
    assert!(session.check().is_err());
    assert!(runtime.tainted.load(Ordering::Acquire));
    assert_eq!(fs::read(path).unwrap(), b"old");
    drop(intent);
    drop(observed);
    drop(inventory);
    drop(lease);
    drop(session);
    drop(runtime);
    drop(connection);
}

#[test]
fn committed_intent_retains_actual_instance_drain_but_never_reopens_source_admission() {
    let f = Fixture::new();
    let (_engine, connection) = f.connection();
    let locations = crate::settings::Locations {
        config: f.base.join("config"),
        data: f.base.join("data"),
        cache: f.base.join("cache"),
    };
    let legacy = f.base.join("legacy");
    fs::create_dir(&legacy).unwrap();
    let profile = crate::settings::Profile::open(locations.clone(), &legacy).unwrap();
    let context =
        PersistentRootContext::from_profile(&connection, profile.source_profile().unwrap())
            .unwrap();
    let runtime = SourceRuntime::new(context, limits()).unwrap();
    let session = runtime.begin().unwrap();
    drop(profile);
    assert!(crate::settings::Profile::open(locations.clone(), &legacy).is_err());
    let path = f.base.join("external.psb");
    fs::write(&path, b"old").unwrap();
    let lease = NativeLease::acquire(&path, || Ok(())).unwrap();
    let inventory = lease
        .freeze_inventory(&session.resident(), &|| Ok(()))
        .unwrap();
    let observed = inventory.observe_external(&|| Ok(())).unwrap();
    let intent = session
        .persist_pending(quota_frame(&session, &observed), &observed, &|| Ok(()))
        .unwrap();
    let control = crate::saving::Control::default();
    control.begin("quota-only-fixture".into(), 7).unwrap();
    // This is the private accounting fixture, not a production prearm/adoption.
    let armed = ArmedPublication {
        closing: super::settlement::Closing::prepare(&intent, &|| Ok(())).unwrap(),
        intent,
        prepared: control.prepare_commit("quota-only-fixture", 7).unwrap(),
    };
    let committed = armed.commit(&control).unwrap();
    committed.check().unwrap();
    assert!(!control.cancel());
    let (done, receive) = std::sync::mpsc::channel();
    let closer = std::thread::spawn(move || {
        drop(connection);
        done.send(()).unwrap()
    });
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
    while session.check().is_ok() && std::time::Instant::now() < deadline {
        std::thread::yield_now()
    }
    assert!(session.check().is_err());
    assert!(runtime.begin().is_err());
    assert!(session.fresh_fragment().is_err());
    assert!(crate::settings::Profile::open(locations.clone(), &legacy).is_err());
    committed.check().unwrap();
    assert!(receive
        .recv_timeout(std::time::Duration::from_millis(30))
        .is_err());
    assert!(session
        .operation
        .check_finalization(&committed.permit, [99; 32])
        .is_err());
    assert_eq!(fs::read(path).unwrap(), b"old");
    drop(committed);
    drop(observed);
    drop(inventory);
    drop(lease);
    drop(session);
    drop(runtime);
    receive
        .recv_timeout(std::time::Duration::from_secs(3))
        .unwrap();
    closer.join().unwrap();
    assert!(crate::settings::Profile::open(locations, &legacy).is_ok());
}

#[test]
fn empty_prearmed_outcome_keeps_pending_growth_but_native_checkpoint_remains_readable() {
    let f = Fixture::new();
    let (_engine, connection) = f.connection();
    let context = PersistentRootContext::from_explicit_state(&connection).unwrap();
    let runtime = SourceRuntime::new(context.clone(), limits()).unwrap();
    let mut engine = crate::engine::Engine::new();
    engine.doc = crate::engine::Document::new_depth(2, 2, 16).unwrap();
    engine.doc.layers[0]
        .pixels
        .set16(0, 0, [12345, 23456, 34567, 0]);
    engine.doc.layers[0]
        .pixels
        .set16(1, 1, [1, 257, 65534, 65535]);
    let cache = crate::engine::scene::LegacyTileCache::new(&runtime.resident(), 4).unwrap();
    let staged = engine
        .capture_scene(None)
        .unwrap()
        .stage(&runtime, &cache, &|| Ok(()))
        .unwrap();
    let session = runtime.begin().unwrap();
    let recovery_limits = crate::recovery::backed::Limits {
        metadata: crate::psd::canonical36::Limits {
            bytes: 65536,
            nodes: 8192,
            depth: 64,
        },
        refs: 64,
        stores: 16,
    };
    let checkpoint = crate::recovery::backed::Capture::new(
        &staged.scene,
        &staged.seals,
        &session,
        recovery_limits,
        &|| Ok(()),
    )
    .unwrap();
    let snapshot = *uuid::Uuid::new_v4().as_bytes();
    checkpoint.publish(snapshot, &session, &|| Ok(())).unwrap();
    let path = f.base.join("external.psb");
    fs::write(&path, b"old").unwrap();
    let lease = NativeLease::acquire(&path, || Ok(())).unwrap();
    let inventory = lease
        .freeze_inventory(&session.resident(), &|| Ok(()))
        .unwrap();
    let observed = inventory.observe_external(&|| Ok(())).unwrap();
    let intent = session
        .persist_pending(quota_frame(&session, &observed), &observed, &|| Ok(()))
        .unwrap();
    let closing = super::settlement::Closing::prepare(&intent, &|| Ok(())).unwrap();
    let debt = runtime.disk().admitted().unwrap() - runtime.disk().used().unwrap();
    assert_eq!(debt, 13);
    drop(closing);
    drop(intent);
    drop(observed);
    drop(inventory);
    drop(lease);
    drop(checkpoint);
    drop(staged);
    drop(cache);
    drop(engine);
    drop(session);
    drop(runtime);
    let restored = SourceRuntime::new(context, limits()).unwrap();
    let session = restored.begin().unwrap();
    assert_eq!(
        restored.disk().admitted().unwrap() - restored.disk().used().unwrap(),
        debt
    );
    let decoded =
        crate::recovery::backed::load(&session, snapshot, recovery_limits, &|| Ok(())).unwrap();
    let (_, source) = decoded.pending.rasters().iter().next().unwrap();
    let region = source
        .prepare_region(
            crate::raster::backed::Region::new([0, 0, 2, 2]).unwrap(),
            &mut || Ok(()),
        )
        .unwrap();
    assert_eq!(region.pixel16(0, 0).unwrap(), [12345, 23456, 34567, 0]);
    assert_eq!(region.pixel16(1, 1).unwrap(), [1, 257, 65534, 65535]);
    assert_eq!(fs::read(path).unwrap(), b"old");
    drop(region);
    drop(decoded);
    drop(session);
    drop(restored);
    drop(connection);
}

fn unchanged_outcome(
    intent: &PublicationIntent<'_>,
    observed: &NativeExternalSnapshot<'_>,
) -> crate::file_safety::AccountingOutcome {
    let mut facts = Vec::new();
    if let Some(fact) = observed.source() {
        facts.push(fact)
    }
    facts.extend(observed.backups().map(|(_, fact)| fact));
    facts.sort_unstable_by_key(|fact| fact.identity());
    crate::file_safety::AccountingOutcome::new(
        intent.sha256(),
        intent.frame.target_key,
        intent.frame.growth,
        facts,
        vec![],
    )
}
#[test]
fn interrupted_outcome_footer_sync_and_selection_keep_native_sources_readable_and_rebuild_debt() {
    use super::settlement::{Closing, Fault};
    for fault in [
        Fault::BeforeFooter,
        Fault::AfterFooter,
        Fault::AfterSync,
        Fault::BeforeSelection,
    ] {
        let f = Fixture::new();
        let (_engine, connection) = f.connection();
        let context = PersistentRootContext::from_explicit_state(&connection).unwrap();
        let runtime = SourceRuntime::new(context.clone(), limits()).unwrap();
        let session = runtime.begin().unwrap();
        let store = session.store_for_lineage([12; 16]).unwrap();
        let native = [0x12u8, 0x34, 0x56, 0x78, 0x9a, 0xbc, 0, 0].repeat(4);
        let block = store
            .put(
                crate::large_document::TileSpec::new(2, 2, 16).unwrap(),
                &native,
                &mut || Ok(()),
            )
            .unwrap();
        let selected = session
            .seal_store(&store, &[block], b"pending native checkpoint read")
            .unwrap();
        let reference = selected.reference();
        let source = store.identity();
        let path = f.base.join("external.psb");
        fs::write(&path, b"old").unwrap();
        let lease = NativeLease::acquire(&path, || Ok(())).unwrap();
        let inventory = lease
            .freeze_inventory(&session.resident(), &|| Ok(()))
            .unwrap();
        let observed = inventory.observe_external(&|| Ok(())).unwrap();
        let intent = session
            .persist_pending(quota_frame(&session, &observed), &observed, &|| Ok(()))
            .unwrap();
        let proof = unchanged_outcome(&intent, &observed);
        let closing = Closing::prepare(&intent, &|| Ok(())).unwrap().inject(fault);
        assert!(closing
            .finish(&intent, &proof, &|| session.check())
            .is_err());
        session.check().unwrap();
        assert_eq!(
            store.pin(block, &mut || Ok(())).unwrap().bytes(),
            native.as_slice()
        );
        assert_eq!(
            runtime.disk().admitted().unwrap() - runtime.disk().used().unwrap(),
            13
        );
        drop(intent);
        drop(observed);
        drop(inventory);
        drop(lease);
        drop(selected);
        drop(store);
        drop(session);
        drop(runtime);
        let restored = SourceRuntime::new(context, limits()).unwrap();
        let session = restored.begin().unwrap();
        let expected = if fault == Fault::BeforeFooter { 13 } else { 3 };
        assert_eq!(
            restored.disk().admitted().unwrap() - restored.disk().used().unwrap(),
            expected
        );
        let selected = session.resolve_store(source, reference).unwrap();
        assert_eq!(
            selected.store().pin(block, &mut || Ok(())).unwrap().bytes(),
            native.as_slice()
        );
        assert_eq!(fs::read(path).unwrap(), b"old");
        drop(selected);
        drop(session);
        drop(restored);
        drop(connection);
    }
}
#[test]
fn durable_physical_creation_token_is_idempotent_exact_session_role_and_file() {
    use crate::file_safety::{AccountingCreation, ExternalCreationRole, NativeExternalFile};
    let f = Fixture::new();
    let (_engine, connection) = f.connection();
    let context = PersistentRootContext::from_explicit_state(&connection).unwrap();
    let runtime = SourceRuntime::new(context, limits()).unwrap();
    let session = runtime.begin().unwrap();
    let path = f.base.join("external.psb");
    fs::write(&path, b"old").unwrap();
    let lease = NativeLease::acquire(&path, || Ok(())).unwrap();
    let inventory = lease
        .freeze_inventory(&session.resident(), &|| Ok(()))
        .unwrap();
    let observed = inventory.observe_external(&|| Ok(())).unwrap();
    let intent = session
        .persist_pending(quota_frame(&session, &observed), &observed, &|| Ok(()))
        .unwrap();
    let plan = intent.plan(&observed, &|| Ok(())).unwrap();
    let temporary = f.base.join(plan.temporary_name());
    let file = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .open(&temporary)
        .unwrap();
    file.sync_all().unwrap();
    let actual = super::settlement::observe(&temporary).unwrap().unwrap();
    let fact =
        NativeExternalFile::for_accounting_tests(actual.identity, actual.bytes, actual.modified);
    let proof = AccountingCreation::new(
        intent.sha256(),
        plan.target_binding(),
        plan.growth(),
        fact,
        ExternalCreationRole::Temporary,
        plan.temporary_name().to_owned(),
        plan.parent_identity(),
    );
    let token = intent.register_creation(&proof, &|| Ok(())).unwrap();
    token
        .check_plan(&plan, &session, ExternalCreationRole::Temporary, fact)
        .unwrap();
    let before = runtime.disk().admitted().unwrap();
    let duplicate = intent.register_creation(&proof, &|| Ok(())).unwrap();
    assert_eq!(runtime.disk().admitted().unwrap(), before);
    assert!(token
        .check_plan(&plan, &session, ExternalCreationRole::Backup, fact)
        .is_err());
    let foreign = runtime.begin().unwrap();
    assert!(token
        .check_plan(&plan, &foreign, ExternalCreationRole::Temporary, fact)
        .is_err());
    drop(foreign);
    assert!(token
        .check_plan(
            &plan,
            &session,
            ExternalCreationRole::Temporary,
            NativeExternalFile::for_accounting_tests(
                (actual.identity.0, actual.identity.1.wrapping_add(1)),
                0,
                actual.modified
            )
        )
        .is_err());
    drop(duplicate);
    drop(token);
    drop(file);
    drop(plan);
    drop(intent);
    drop(observed);
    drop(inventory);
    drop(lease);
    drop(session);
    drop(runtime);
    drop(connection);
}
