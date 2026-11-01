use super::*;
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Barrier,
};

struct Fixture {
    base: PathBuf,
    directory: Option<File>,
    identity: (u64, u64),
}
impl Fixture {
    fn new() -> Self {
        let base = std::env::temp_dir().join(format!(
            "peerbrush-native-block-tests-{}",
            uuid::Uuid::new_v4()
        ));
        fs::create_dir(&base).unwrap();
        let base = fs::canonicalize(base).unwrap();
        let directory = open_directory(&base).unwrap();
        let identity = crate::file_safety::identity(&directory).unwrap();
        Self {
            base,
            directory: Some(directory),
            identity,
        }
    }
    fn root(&self) -> PathBuf {
        self.base.join("store")
    }
    fn store(&self, budget: &ResidentBudget) -> Store {
        Store::create(&self.root(), budget.clone(), limits()).unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        // Only the uniquely created fixture beneath the actual temp root is cleaned.
        if let Ok(temp) = fs::canonicalize(std::env::temp_dir()) {
            if self.base.starts_with(&temp)
                && self.base.file_name().is_some_and(|n| {
                    n.to_string_lossy()
                        .starts_with("peerbrush-native-block-tests-")
                })
                && validate_ancestors(&self.base).is_ok()
                && fs::canonicalize(&self.base).is_ok_and(|actual| actual == self.base)
                && open_directory(&self.base)
                    .and_then(|file| {
                        crate::file_safety::identity(&file).map_err(|_| Error::Changed)
                    })
                    .is_ok_and(|identity| identity == self.identity)
                && self.directory.as_ref().is_some_and(|file| {
                    crate::file_safety::identity(file)
                        .is_ok_and(|identity| identity == self.identity)
                })
            {
                drop(self.directory.take());
                let _ = fs::remove_dir_all(&self.base);
            }
        }
    }
}
fn limits() -> Limits {
    Limits {
        disk_bytes: 16 * 1024 * 1024,
        max_blocks: 4096,
        max_resident_blocks: 64,
        manifest_bytes: 1024 * 1024,
    }
}
fn okay() -> Result<()> {
    Ok(())
}
fn bytes(spec: TileSpec, seed: u16) -> Vec<u8> {
    if spec.depth == 8 {
        (0..spec.bytes())
            .map(|i| (i as u16).wrapping_mul(37).wrapping_add(seed) as u8)
            .collect()
    } else {
        (0..spec.bytes() / 2)
            .flat_map(|i| {
                (i as u16)
                    .wrapping_mul(177)
                    .wrapping_add(seed)
                    .to_be_bytes()
            })
            .collect()
    }
}
fn owned_bytes(store: &Store) -> Vec<u8> {
    let mut bytes = vec![0; store.0.file.metadata().unwrap().len() as usize];
    read_exact_at(&store.0.file, 0, &mut bytes).unwrap();
    bytes
}

#[test]
fn index_capacity_stays_charged_through_shared_versions_until_the_last_source_owner() {
    let fixture = Fixture::new();
    let budget = ResidentBudget::new(4096).unwrap();
    let store = fixture.store(&budget);
    let block = store
        .put(TileSpec::new(1, 1, 8).unwrap(), &[7; 4], &mut okay)
        .unwrap();
    let after_block = budget.used();
    assert!(after_block > 0);
    assert_eq!(budget.payload_used(), 0);
    let snapshot = store
        .commit([9; 16], 1, None, b"source", &[block], &mut okay)
        .unwrap();
    let version = snapshot.clone();
    assert_eq!(budget.used(), after_block);
    let pin = snapshot.metadata(&mut okay).unwrap();
    assert_eq!(budget.payload_used(), 6);
    drop(pin);
    let index_bytes = budget.used();
    assert!(index_bytes > after_block);
    assert_eq!(budget.payload_used(), 0);
    drop(store);
    drop(snapshot);
    assert_eq!(budget.used(), index_bytes);
    drop(version);
    assert_eq!(budget.used(), 0);
}

#[test]
fn index_growth_refusal_precedes_source_append_and_preserves_all_old_bytes() {
    let fixture = Fixture::new();
    let one_index = 2 * std::mem::size_of::<(Digest, BlockRef)>();
    let budget = ResidentBudget::new(one_index).unwrap();
    let store = fixture.store(&budget);
    let spec = TileSpec::new(1, 1, 8).unwrap();
    let first = store.put(spec, &[7; 4], &mut okay).unwrap();
    let before = owned_bytes(&store);
    assert!(matches!(
        store.put(spec, &[8; 4], &mut okay),
        Err(Error::Budget)
    ));
    assert_eq!(owned_bytes(&store), before);
    assert_eq!(
        store.state().unwrap().blocks.get(&first.digest),
        Some(&first)
    );
    assert_eq!(budget.used(), one_index);
    assert_eq!(budget.payload_used(), 0);
    drop(store);
    assert_eq!(budget.used(), 0);
}

#[test]
fn recovered_prefix_index_admission_precedes_payload_ingestion_and_releases_on_refusal() {
    let fixture = Fixture::new();
    let original = ResidentBudget::new(4096).unwrap();
    let store = fixture.store(&original);
    let spec = TileSpec::new(1, 1, 8).unwrap();
    let mut blocks = Vec::new();
    for byte in 0..3 {
        blocks.push(store.put(spec, &[byte; 4], &mut okay).unwrap());
    }
    let snapshot = store
        .commit([3; 16], 1, None, b"selected", &blocks, &mut okay)
        .unwrap();
    let selected = snapshot.reference();
    // Invalid payload cannot become authority; low metadata capacity refuses
    // before the subsequent mandatory full integrity pass attempts ingestion.
    write_all_at(&store.0.file, blocks[0].offset + RECORD_HEADER + 8, &[99]).unwrap();
    drop(snapshot);
    drop(store);
    assert_eq!(original.used(), 0);
    let minimum = 2 * 3 * std::mem::size_of::<(Digest, BlockRef)>();
    let budget = ResidentBudget::new(minimum - 1).unwrap();
    assert!(matches!(
        Store::recover(
            &fixture.root(),
            selected,
            budget.clone(),
            limits(),
            &mut okay
        ),
        Err(Error::Budget)
    ));
    assert_eq!(budget.used(), 0);
    let enough = ResidentBudget::new(4096).unwrap();
    assert!(matches!(
        Store::recover(
            &fixture.root(),
            selected,
            enough.clone(),
            limits(),
            &mut okay
        ),
        Err(Error::Corrupt)
    ));
    assert_eq!(enough.used(), 0);
}

#[test]
fn cache_index_refusal_returns_candidate_payload_capacity_without_freeing_source_metadata() {
    let fixture = Fixture::new();
    let metadata = 2 * std::mem::size_of::<(Digest, BlockRef)>();
    let budget = ResidentBudget::new(metadata + 256).unwrap();
    let store = fixture.store(&budget);
    let block = store
        .put(TileSpec::new(8, 8, 8).unwrap(), &[9; 256], &mut okay)
        .unwrap();
    let before = owned_bytes(&store);
    assert!(matches!(store.pin(block, &mut okay), Err(Error::Budget)));
    assert_eq!(budget.payload_used(), 0);
    assert_eq!(budget.used(), metadata);
    assert_eq!(owned_bytes(&store), before);
    drop(store);
    assert_eq!(budget.used(), 0);
}

#[test]
fn unpublished_fragment_abort_verifies_all_records_then_releases_only_its_charge() {
    for depth in [8, 16] {
        let fixture = Fixture::new();
        let resident = ResidentBudget::new(1024 * 1024).unwrap();
        let disk = DiskBudget::new(1024 * 1024, 4).unwrap();
        let fragment =
            FreshFragment::create(&fixture.root(), resident.clone(), disk.clone(), limits())
                .unwrap();
        let spec = TileSpec::new(17, 9, depth).unwrap();
        let first = fragment
            .store()
            .put(spec, &bytes(spec, 4), &mut okay)
            .unwrap();
        let snapshot = fragment
            .store()
            .commit([8; 16], 1, None, b"candidate", &[first], &mut okay)
            .unwrap();
        drop(snapshot);
        fragment
            .store()
            .put(spec, &bytes(spec, 5), &mut okay)
            .unwrap();
        assert!(disk.used().unwrap() > 24);
        fragment.abort(&mut okay).unwrap();
        assert!(!fixture.root().join("blocks.pbt").exists());
        assert!(fixture.root().is_dir());
        assert_eq!(disk.used().unwrap(), 0);
        assert_eq!(resident.used(), 0);
    }
}

#[test]
fn fragment_publication_or_a_retained_candidate_prevents_early_retirement() {
    for published in [false, true] {
        let fixture = Fixture::new();
        let disk = DiskBudget::new(1024 * 1024, 4).unwrap();
        let fragment = FreshFragment::create(
            &fixture.root(),
            ResidentBudget::new(1024 * 1024).unwrap(),
            disk.clone(),
            limits(),
        )
        .unwrap();
        let candidate = fragment.store().clone();
        if published {
            let store = fragment.publish();
            assert!(candidate.0.may_be_published.load(Ordering::Acquire));
            drop(candidate);
            assert!(matches!(
                store.retire_unpublished(&mut okay),
                Err(Error::Invalid(_))
            ));
        } else {
            assert!(matches!(fragment.abort(&mut okay), Err(Error::Busy)));
            drop(candidate);
        }
        assert_eq!(
            fs::metadata(fixture.root().join("blocks.pbt"))
                .unwrap()
                .len(),
            24
        );
        assert_eq!(disk.used().unwrap(), 24);
    }
}

#[test]
fn dropped_or_recovered_fragments_never_gain_unpublished_retirement_authority() {
    let fixture = Fixture::new();
    let resident = ResidentBudget::new(1024 * 1024).unwrap();
    let disk = DiskBudget::new(1024 * 1024, 4).unwrap();
    let fragment =
        FreshFragment::create(&fixture.root(), resident.clone(), disk.clone(), limits()).unwrap();
    let snapshot = fragment
        .store()
        .commit([7; 16], 0, None, b"staged", &[], &mut okay)
        .unwrap();
    let selected = snapshot.reference();
    drop(snapshot);
    let charged = disk.used().unwrap();
    drop(fragment);
    assert_eq!(disk.used().unwrap(), charged);
    let restored = Store::recover_with_disk_budget(
        &fixture.root(),
        selected,
        resident,
        disk.clone(),
        limits(),
        &mut okay,
    )
    .unwrap();
    let store = restored.store();
    drop(restored);
    assert!(matches!(
        store.retire_unpublished(&mut okay),
        Err(Error::Invalid(_))
    ));
    assert_eq!(disk.used().unwrap(), charged);
    assert_eq!(
        fs::metadata(fixture.root().join("blocks.pbt"))
            .unwrap()
            .len(),
        charged
    );
}

#[test]
fn fragment_abort_keeps_canceled_corrupt_partial_and_foreign_bytes_charged() {
    for fault in 0..4 {
        let fixture = Fixture::new();
        let disk = DiskBudget::new(1024 * 1024, 4).unwrap();
        let fragment = FreshFragment::create(
            &fixture.root(),
            ResidentBudget::new(1024 * 1024).unwrap(),
            disk.clone(),
            limits(),
        )
        .unwrap();
        let spec = TileSpec::new(4, 3, 16).unwrap();
        fragment
            .store()
            .put(spec, &bytes(spec, 9), &mut okay)
            .unwrap();
        let path = fixture.root().join("blocks.pbt");
        let reserved = disk.used().unwrap();
        match fault {
            0 => {
                assert!(matches!(
                    fragment.abort(&mut || Err(Error::Cancelled)),
                    Err(Error::Cancelled)
                ));
            }
            1 => {
                write_all_at(
                    &fragment.store().0.file,
                    FILE_HEADER + RECORD_HEADER + 8,
                    &[42],
                )
                .unwrap();
                fragment.store().state().unwrap().expected =
                    stamp(&fragment.store().0.file).unwrap();
                assert!(matches!(fragment.abort(&mut okay), Err(Error::Corrupt)));
            }
            2 => {
                fragment.store().state().unwrap().poisoned = true;
                assert!(matches!(fragment.abort(&mut okay), Err(Error::Interrupted)));
            }
            _ => {
                fs::rename(&path, fixture.root().join("held.pbt")).unwrap();
                fs::write(&path, b"foreign").unwrap();
                assert!(matches!(fragment.abort(&mut okay), Err(Error::Changed)));
                assert_eq!(fs::read(&path).unwrap(), b"foreign");
                assert_eq!(
                    fs::metadata(fixture.root().join("held.pbt")).unwrap().len(),
                    reserved
                );
            }
        }
        assert!(path.exists());
        assert_eq!(disk.used().unwrap(), reserved);
    }
}

#[test]
fn fragment_abort_rechecks_changes_after_its_final_callback() {
    let mut calls = 0;
    for late_change in [false, true] {
        let fixture = Fixture::new();
        let disk = DiskBudget::new(1024 * 1024, 4).unwrap();
        let fragment = FreshFragment::create(
            &fixture.root(),
            ResidentBudget::new(1024 * 1024).unwrap(),
            disk.clone(),
            limits(),
        )
        .unwrap();
        let spec = TileSpec::new(2, 2, 8).unwrap();
        fragment
            .store()
            .put(spec, &bytes(spec, 1), &mut okay)
            .unwrap();
        let path = fixture.root().join("blocks.pbt");
        let reserved = disk.used().unwrap();
        let mut at = 0;
        let result = fragment.abort(&mut || {
            at += 1;
            if late_change && at == calls {
                fs::rename(&path, fixture.root().join("late-held.pbt"))?;
                fs::write(&path, b"late foreign")?;
            }
            Ok(())
        });
        if late_change {
            assert!(matches!(result, Err(Error::Changed)));
            assert_eq!(fs::read(path).unwrap(), b"late foreign");
            assert_eq!(
                fs::metadata(fixture.root().join("late-held.pbt"))
                    .unwrap()
                    .len(),
                reserved
            );
            assert_eq!(disk.used().unwrap(), reserved);
        } else {
            result.unwrap();
            calls = at;
            assert!(calls >= 3);
            assert_eq!(disk.used().unwrap(), 0);
        }
    }
}

#[test]
fn native_depth_blocks_deduplicate_share_capacity_and_recover_exact_checkpoint() {
    for depth in [8, 16] {
        let fixture = Fixture::new();
        let budget = ResidentBudget::new(2 * 1024 * 1024).unwrap();
        let store = fixture.store(&budget);
        let spec = TileSpec::new(33, 17, depth).unwrap();
        let source = bytes(spec, 10001);
        let block = store.put(spec, &source, &mut okay).unwrap();
        let length = store.0.file.metadata().unwrap().len();
        assert_eq!(store.put(spec, &source, &mut okay).unwrap(), block);
        assert_eq!(store.0.file.metadata().unwrap().len(), length);
        let first = store.pin(block, &mut okay).unwrap();
        let second = store.pin(block, &mut okay).unwrap();
        assert_eq!(first.bytes(), source);
        assert_eq!(first.spec(), spec);
        assert!(Arc::ptr_eq(&first.bytes.0, &second.bytes.0));
        assert_eq!(budget.payload_used(), first.allocated_bytes());
        drop(first);
        assert_eq!(budget.payload_used(), second.allocated_bytes());
        drop(second);
        assert_eq!(budget.payload_used(), 0);
        let snapshot = store
            .commit(
                [7; 16],
                0,
                None,
                b"opaque source metadata",
                &[block],
                &mut okay,
            )
            .unwrap();
        let selected = snapshot.reference();
        assert_eq!(CommitRef::decode(&selected.encode()).unwrap(), selected);
        let metadata = snapshot.metadata(&mut okay).unwrap();
        assert_eq!(&*metadata, b"opaque source metadata");
        assert_eq!(budget.payload_used(), metadata.allocated_bytes());
        drop(metadata);
        assert_eq!(budget.payload_used(), 0);
        let mut refs = Vec::new();
        snapshot
            .visit_blocks(&mut okay, |reference| {
                refs.push(reference);
                Ok(())
            })
            .unwrap();
        assert_eq!(refs, [block]);
        drop(snapshot);
        drop(store);
        let recovered = Store::recover(
            &fixture.root(),
            selected,
            budget.clone(),
            limits(),
            &mut okay,
        )
        .unwrap();
        assert!(!recovered.has_unconfirmed_tail().unwrap());
        assert_eq!(
            &*recovered.metadata(&mut okay).unwrap(),
            b"opaque source metadata"
        );
        assert_eq!(
            recovered.store.pin(block, &mut okay).unwrap().bytes(),
            source
        );
        let next = recovered
            .store
            .commit([7; 16], 1, Some(selected), b"next", &[block], &mut okay)
            .unwrap();
        assert_eq!(next.reference().revision(), 1);
    }
}

#[test]
fn depth_dimensions_store_identity_and_encoded_lengths_are_bound() {
    assert!(TileSpec::new(0, 1, 8).is_err());
    assert!(TileSpec::new(257, 1, 8).is_err());
    assert!(TileSpec::new(1, 1, 32).is_err());
    let fixture = Fixture::new();
    let budget = ResidentBudget::new(1024).unwrap();
    let store = fixture.store(&budget);
    let spec = TileSpec::new(2, 1, 8).unwrap();
    assert!(store.put(spec, &[0; 7], &mut okay).is_err());
    let block = store.put(spec, &[0; 8], &mut okay).unwrap();
    let mut wrong = block;
    wrong.spec = TileSpec::new(1, 1, 16).unwrap();
    assert!(matches!(store.pin(wrong, &mut okay), Err(Error::Corrupt)));
    wrong = block;
    wrong.store[0] ^= 1;
    assert!(matches!(store.pin(wrong, &mut okay), Err(Error::Corrupt)));
    wrong = block;
    wrong.offset += 1;
    assert!(matches!(store.pin(wrong, &mut okay), Err(Error::Corrupt)));
    let mut serialized = [0; 96];
    serialized[..8].copy_from_slice(b"PBREF001");
    assert!(CommitRef::decode(&serialized).is_err());
    assert_eq!(budget.payload_used(), 0);
}

#[test]
fn stale_source_parent_and_foreign_block_cannot_append() {
    let fixture = Fixture::new();
    let budget = ResidentBudget::new(1024).unwrap();
    let store = fixture.store(&budget);
    let block = store
        .put(TileSpec::new(1, 1, 8).unwrap(), &[1; 4], &mut okay)
        .unwrap();
    let snapshot = store
        .commit([1; 16], 5, None, b"first", &[block], &mut okay)
        .unwrap();
    let parent = snapshot.reference();
    let original = owned_bytes(&store);
    for (source, revision, parent) in [
        ([1; 16], 5, Some(parent)),
        ([1; 16], 6, None),
        ([2; 16], 6, Some(parent)),
    ] {
        assert!(matches!(
            store.commit(source, revision, parent, b"wrong", &[block], &mut okay),
            Err(Error::Stale)
        ));
        assert_eq!(owned_bytes(&store), original);
    }
    let mut foreign = block;
    foreign.store[0] ^= 1;
    assert!(matches!(
        store.commit([1; 16], 6, Some(parent), b"wrong", &[foreign], &mut okay),
        Err(Error::Corrupt)
    ));
    assert_eq!(owned_bytes(&store), original);
}

#[test]
fn failed_and_cancelled_loads_restore_the_shared_capacity() {
    let fixture = Fixture::new();
    let budget = ResidentBudget::new(1024).unwrap();
    let store = fixture.store(&budget);
    let block = store
        .put(TileSpec::new(8, 8, 8).unwrap(), &[19; 256], &mut okay)
        .unwrap();
    super::super::budget::refuse_next_allocation();
    assert!(matches!(store.pin(block, &mut okay), Err(Error::Budget)));
    assert_eq!(budget.payload_used(), 0);
    let mut calls = 0;
    let result = store.pin(block, &mut || {
        calls += 1;
        if calls == 2 {
            Err(Error::Cancelled)
        } else {
            Ok(())
        }
    });
    assert!(matches!(result, Err(Error::Cancelled)));
    assert_eq!(budget.payload_used(), 0);
    let result = store.load((3, [7; 32]), 256, &mut okay, |_, _| {
        Err(Error::Io(std::io::ErrorKind::Other.into()))
    });
    assert!(matches!(result, Err(Error::Io(_))));
    assert_eq!(budget.payload_used(), 0);
    assert_eq!(store.pin(block, &mut okay).unwrap().bytes(), [19; 256]);
    assert_eq!(budget.payload_used(), 0);
}

#[test]
fn no_weak_entry_can_keep_an_uncharged_live_payload() {
    let fixture = Fixture::new();
    let budget = ResidentBudget::new(256 + 288).unwrap();
    let store = fixture.store(&budget);
    let block = store
        .put(TileSpec::new(8, 8, 8).unwrap(), &[23; 256], &mut okay)
        .unwrap();
    let pin = store.pin(block, &mut okay).unwrap();
    let clone = pin.clone();
    assert_eq!(budget.payload_used(), 256);
    drop(pin);
    assert_eq!(budget.payload_used(), 256);
    drop(clone);
    assert_eq!(budget.payload_used(), 0);
    assert_eq!(store.0.cache.lock().unwrap().len(), 1);
    assert_eq!(store.pin(block, &mut okay).unwrap().bytes(), [23; 256]);
    assert_eq!(budget.payload_used(), 0);
}

#[test]
fn slot_and_byte_admission_preserve_other_pins() {
    let fixture = Fixture::new();
    let budget = ResidentBudget::new(512 + 480).unwrap();
    let mut admitted = limits();
    admitted.max_resident_blocks = 1;
    let store = Store::create(&fixture.root(), budget.clone(), admitted).unwrap();
    let spec = TileSpec::new(8, 8, 8).unwrap();
    let a = store.put(spec, &[1; 256], &mut okay).unwrap();
    let b = store.put(spec, &[2; 256], &mut okay).unwrap();
    let first = store.pin(a, &mut okay).unwrap();
    assert!(matches!(store.pin(b, &mut okay), Err(Error::Budget)));
    assert_eq!(first.bytes(), [1; 256]);
    assert_eq!(budget.payload_used(), 256);
    drop(first);
    assert_eq!(budget.payload_used(), 0);
    assert_eq!(store.pin(b, &mut okay).unwrap().bytes(), [2; 256]);
    assert_eq!(budget.payload_used(), 0);
}

#[test]
fn mutable_scratch_freezes_without_copy_and_keeps_one_live_charge() {
    let budget = ResidentBudget::new(1024).unwrap();
    let mut scratch = ScratchBytes::new(&budget, 512, 512).unwrap();
    assert_eq!(budget.payload_used(), 512);
    assert!(scratch.iter().all(|value| *value == 0));
    scratch[..2].copy_from_slice(&12347u16.to_be_bytes());
    let pointer = scratch.as_ptr();
    let pinned = scratch.freeze();
    assert!(pinned.uses_budget(&budget));
    assert!(!pinned.uses_budget(&ResidentBudget::new(1024).unwrap()));
    assert_eq!(pointer, pinned.as_ptr());
    assert_eq!(&pinned[..2], &12347u16.to_be_bytes());
    let clone = pinned.clone();
    assert_eq!(budget.payload_used(), 512);
    drop(pinned);
    assert_eq!(budget.payload_used(), 512);
    drop(clone);
    assert_eq!(budget.payload_used(), 0);
    assert!(matches!(
        ScratchBytes::new(&budget, 513, 512),
        Err(Error::Budget)
    ));
    assert!(matches!(
        ScratchBytes::new(&budget, 0, 64 * 1024 * 1024 + 1),
        Err(Error::Budget)
    ));
    assert_eq!(budget.payload_used(), 0);
    super::super::budget::refuse_next_allocation();
    assert!(matches!(
        ScratchBytes::new(&budget, 512, 512),
        Err(Error::Budget)
    ));
    assert_eq!(budget.payload_used(), 0);
}

#[test]
fn external_reservations_and_store_pins_use_the_same_pool() {
    let fixture = Fixture::new();
    let budget = ResidentBudget::new(512 + 288).unwrap();
    let store = fixture.store(&budget);
    let pool = store.resident_budget();
    let reserved = pool.reserve_bytes(256, 256).unwrap();
    assert_eq!(reserved.bytes(), 256);
    assert_eq!(budget.payload_used(), 256);
    let block = store
        .put(TileSpec::new(8, 8, 8).unwrap(), &[21; 256], &mut okay)
        .unwrap();
    let pin = store.pin(block, &mut okay).unwrap();
    assert_eq!(budget.payload_used(), 512);
    assert!(matches!(pool.reserve_bytes(1, 1), Err(Error::Budget)));
    drop(reserved);
    assert_eq!(budget.payload_used(), 256);
    drop(pin);
    assert_eq!(budget.payload_used(), 0);
    assert!(matches!(pool.reserve_bytes(257, 256), Err(Error::Budget)));
    assert!(matches!(
        pool.reserve_bytes(0, 64 * 1024 * 1024 + 1),
        Err(Error::Budget)
    ));
}

#[test]
fn concurrent_duplicate_loads_charge_both_then_keep_one_shared_allocation() {
    concurrent_pins(true);
}
#[test]
fn concurrent_different_loads_keep_both_allocations_charged() {
    concurrent_pins(false);
}
fn concurrent_pins(duplicate: bool) {
    let fixture = Fixture::new();
    let budget = ResidentBudget::new(32768 + 1024).unwrap();
    let store = fixture.store(&budget);
    let spec = TileSpec::new(64, 64, 8).unwrap();
    let a = store.put(spec, &[3; 16384], &mut okay).unwrap();
    let b = if duplicate {
        a
    } else {
        store.put(spec, &[4; 16384], &mut okay).unwrap()
    };
    let arrivals = Arc::new(AtomicUsize::new(0));
    let barrier = Arc::new(Barrier::new(3));
    let mut workers = Vec::new();
    for block in [a, b] {
        let source = store.clone();
        let arrivals = arrivals.clone();
        let barrier = barrier.clone();
        workers.push(std::thread::spawn(move || {
            let mut calls = 0;
            source
                .pin(block, &mut || {
                    // A competing loader may briefly hold either mutex. The
                    // isolated reentrant-callback test below proves that this
                    // callback never owns them itself.
                    calls += 1;
                    if calls == 2 {
                        arrivals.fetch_add(1, Ordering::Release);
                        barrier.wait();
                    }
                    Ok(())
                })
                .unwrap()
        }));
    }
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while arrivals.load(Ordering::Acquire) != 2 {
        assert!(std::time::Instant::now() < deadline);
        std::thread::yield_now();
    }
    assert_eq!(budget.payload_used(), 32768);
    barrier.wait();
    let first = workers.remove(0).join().unwrap();
    let second = workers.remove(0).join().unwrap();
    assert_eq!(first.bytes(), [3; 16384]);
    assert_eq!(
        second.bytes(),
        if duplicate { [3; 16384] } else { [4; 16384] }
    );
    assert_eq!(Arc::ptr_eq(&first.bytes.0, &second.bytes.0), duplicate);
    assert_eq!(budget.payload_used(), if duplicate { 16384 } else { 32768 });
    drop(first);
    assert_eq!(budget.payload_used(), 16384);
    drop(second);
    assert_eq!(budget.payload_used(), 0);
}

#[test]
fn callbacks_can_reenter_admission_without_cache_or_state_deadlock() {
    let fixture = Fixture::new();
    let budget = ResidentBudget::new(1024).unwrap();
    let store = fixture.store(&budget);
    let spec = TileSpec::new(8, 8, 8).unwrap();
    let block = store.put(spec, &[1; 256], &mut okay).unwrap();
    let pin = store
        .pin(block, &mut || {
            assert!(store.0.cache.try_lock().is_ok());
            assert!(store.0.state.try_lock().is_ok());
            assert!(matches!(
                store.put(spec, &[2; 256], &mut okay),
                Err(Error::Busy)
            ));
            Ok(())
        })
        .unwrap();
    assert_eq!(pin.bytes(), [1; 256]);
}

#[test]
fn cancelled_append_keeps_checkpoint_and_marks_unknown_tail_readonly_on_recovery() {
    let fixture = Fixture::new();
    let budget = ResidentBudget::new(1024).unwrap();
    let store = fixture.store(&budget);
    let spec = TileSpec::new(8, 8, 8).unwrap();
    let block = store.put(spec, &[1; 256], &mut okay).unwrap();
    let snapshot = store
        .commit([9; 16], 0, None, b"retained", &[block], &mut okay)
        .unwrap();
    let selected = snapshot.reference();
    let before = owned_bytes(&store);
    assert!(!snapshot.has_unconfirmed_tail().unwrap());
    let result = store.put(spec, &[2; 256], &mut || {
        if store.0.file.metadata()?.len() > before.len() as u64 {
            Err(Error::Cancelled)
        } else {
            Ok(())
        }
    });
    assert!(matches!(result, Err(Error::Cancelled)));
    assert!(snapshot.has_unconfirmed_tail().unwrap());
    assert_eq!(&owned_bytes(&store)[..before.len()], before);
    assert!(matches!(
        store.put(spec, &[3; 256], &mut okay),
        Err(Error::Interrupted)
    ));
    assert_eq!(store.pin(block, &mut okay).unwrap().bytes(), [1; 256]);
    drop(snapshot);
    drop(store);
    let recovered = Store::recover(
        &fixture.root(),
        selected,
        budget.clone(),
        limits(),
        &mut okay,
    )
    .unwrap();
    assert!(recovered.has_unconfirmed_tail().unwrap());
    assert_eq!(&*recovered.metadata(&mut okay).unwrap(), b"retained");
    assert!(matches!(
        recovered.store.commit(
            [9; 16],
            1,
            Some(selected),
            b"no replay",
            &[block],
            &mut okay
        ),
        Err(Error::Interrupted)
    ));
    assert_eq!(budget.payload_used(), 0);
}

#[test]
fn cancelled_complete_seal_is_not_silently_promoted_or_replayed() {
    let fixture = Fixture::new();
    let budget = ResidentBudget::new(1024).unwrap();
    let store = fixture.store(&budget);
    let block = store
        .put(TileSpec::new(1, 1, 8).unwrap(), &[1; 4], &mut okay)
        .unwrap();
    let snapshot = store
        .commit([8; 16], 0, None, b"old", &[block], &mut okay)
        .unwrap();
    let selected = snapshot.reference();
    let expected_end =
        selected.end + RECORD_HEADER + (MANIFEST_HEADER + 3 + REF_BYTES) as u64 + SEAL.len() as u64;
    let result = store.commit([8; 16], 1, Some(selected), b"new", &[block], &mut || {
        if store.0.file.metadata()?.len() == expected_end {
            Err(Error::Cancelled)
        } else {
            Ok(())
        }
    });
    assert!(matches!(result, Err(Error::Cancelled)));
    assert_eq!(store.state().unwrap().head, Some(selected));
    assert!(snapshot.has_unconfirmed_tail().unwrap());
    drop(snapshot);
    drop(store);
    let recovered = Store::recover(&fixture.root(), selected, budget, limits(), &mut okay).unwrap();
    assert!(recovered.has_unconfirmed_tail().unwrap());
    assert_eq!(&*recovered.metadata(&mut okay).unwrap(), b"old");
}

#[test]
fn corrupt_and_truncated_payloads_return_errors_without_a_resident_leak() {
    for truncated in [false, true] {
        let fixture = Fixture::new();
        let budget = ResidentBudget::new(1024).unwrap();
        let store = fixture.store(&budget);
        let block = store
            .put(TileSpec::new(8, 8, 8).unwrap(), &[77; 256], &mut okay)
            .unwrap();
        if truncated {
            store
                .0
                .file
                .set_len(block.offset + RECORD_HEADER + 8 + 128)
                .unwrap();
        } else {
            write_all_at(&store.0.file, block.offset + RECORD_HEADER + 8, &[19]).unwrap();
        }
        // Simulate corruption without relying on a timestamp change to catch it.
        store.state().unwrap().expected = stamp(&store.0.file).unwrap();
        assert!(matches!(store.pin(block, &mut okay), Err(Error::Corrupt)));
        assert_eq!(budget.payload_used(), 0);
    }
}

#[test]
fn metadata_and_reference_bytes_keep_their_original_sha_binding() {
    let fixture = Fixture::new();
    let budget = ResidentBudget::new(1024).unwrap();
    let store = fixture.store(&budget);
    let spec = TileSpec::new(1, 1, 8).unwrap();
    let a = store.put(spec, &[1; 4], &mut okay).unwrap();
    let b = store.put(spec, &[2; 4], &mut okay).unwrap();
    let snapshot = store
        .commit([1; 16], 0, None, b"native metadata", &[a], &mut okay)
        .unwrap();
    write_all_at(&store.0.file, snapshot.metadata.0, &[0]).unwrap();
    store.state().unwrap().expected = stamp(&store.0.file).unwrap();
    assert!(matches!(snapshot.metadata(&mut okay), Err(Error::Corrupt)));
    assert_eq!(budget.payload_used(), 0);
    write_all_at(&store.0.file, snapshot.blocks.0, &b.encode()).unwrap();
    store.state().unwrap().expected = stamp(&store.0.file).unwrap();
    assert!(matches!(
        snapshot.visit_blocks(&mut okay, |_| Ok(())),
        Err(Error::Corrupt)
    ));
}

#[test]
fn foreign_replacement_is_never_overwritten_or_deleted() {
    let fixture = Fixture::new();
    let budget = ResidentBudget::new(1024).unwrap();
    let store = fixture.store(&budget);
    let spec = TileSpec::new(1, 1, 8).unwrap();
    let block = store.put(spec, &[1; 4], &mut okay).unwrap();
    let path = store.0.path.clone();
    fs::rename(&path, fixture.base.join("retained-owned-pack")).unwrap();
    fs::write(&path, b"foreign replacement").unwrap();
    assert!(matches!(store.pin(block, &mut okay), Err(Error::Changed)));
    assert!(matches!(
        store.put(spec, &[2; 4], &mut okay),
        Err(Error::Changed)
    ));
    drop(store);
    assert_eq!(fs::read(path).unwrap(), b"foreign replacement");
}

#[test]
fn alias_and_existing_root_refusal_preserve_original_contents() {
    let fixture = Fixture::new();
    let budget = ResidentBudget::new(1024).unwrap();
    let store = fixture.store(&budget);
    let path = store.0.path.clone();
    let original = owned_bytes(&store);
    assert!(Store::create(&fixture.root(), budget.clone(), limits()).is_err());
    assert_eq!(owned_bytes(&store), original);
    fs::hard_link(&path, fixture.base.join("foreign-alias")).unwrap();
    assert!(matches!(
        store.put(TileSpec::new(1, 1, 8).unwrap(), &[1; 4], &mut okay),
        Err(Error::Changed)
    ));
    drop(store);
    assert_eq!(fs::read(path).unwrap(), original);
    assert_eq!(
        fs::read(fixture.base.join("foreign-alias")).unwrap(),
        original
    );
}

#[test]
fn disk_and_record_admission_refuse_before_appending_any_bytes() {
    let fixture = Fixture::new();
    let budget = ResidentBudget::new(1024).unwrap();
    let mut small = limits();
    small.disk_bytes = FILE_HEADER + RECORD_HEADER + 8 + 4 + SEAL.len() as u64;
    small.max_blocks = 1;
    small.max_resident_blocks = 1;
    let store = Store::create(&fixture.root(), budget, small).unwrap();
    let spec = TileSpec::new(1, 1, 8).unwrap();
    let block = store.put(spec, &[1; 4], &mut okay).unwrap();
    let original = owned_bytes(&store);
    assert_eq!(store.put(spec, &[1; 4], &mut okay).unwrap(), block);
    assert!(matches!(
        store.put(spec, &[2; 4], &mut okay),
        Err(Error::Budget)
    ));
    assert_eq!(owned_bytes(&store), original);
    assert!(matches!(
        store.commit([1; 16], 0, None, b"metadata", &[block], &mut okay),
        Err(Error::Budget)
    ));
    assert_eq!(owned_bytes(&store), original);
}

#[test]
fn actual_partial_io_failure_preserves_confirmed_prefix_and_forbids_retry() {
    let fixture = Fixture::new();
    let budget = ResidentBudget::new(1024).unwrap();
    let store = fixture.store(&budget);
    let original = owned_bytes(&store);
    let _write = store.0.access.write().unwrap();
    let result = store.append_with(b"TILE", 12, [1; 32], &mut okay, |offset, _| {
        write_all_at(&store.0.file, offset, &[1, 2])?;
        Err(Error::Io(std::io::ErrorKind::StorageFull.into()))
    });
    assert!(matches!(result, Err(Error::Io(_))));
    assert_eq!(&owned_bytes(&store)[..original.len()], original);
    drop(_write);
    assert!(matches!(
        store.put(TileSpec::new(1, 1, 8).unwrap(), &[1; 4], &mut okay),
        Err(Error::Interrupted)
    ));
}

#[test]
fn insufficient_concurrent_capacity_never_allocates_an_uncharged_duplicate() {
    let fixture = Fixture::new();
    let budget = ResidentBudget::new(256 + 288).unwrap();
    let store = fixture.store(&budget);
    let block = store
        .put(TileSpec::new(8, 8, 8).unwrap(), &[31; 256], &mut okay)
        .unwrap();
    let entered = Arc::new(AtomicUsize::new(0));
    let barrier = Arc::new(Barrier::new(2));
    let worker_store = store.clone();
    let signal = entered.clone();
    let release = barrier.clone();
    let worker = std::thread::spawn(move || {
        let mut calls = 0;
        worker_store
            .pin(block, &mut || {
                calls += 1;
                if calls == 2 {
                    signal.store(1, Ordering::Release);
                    release.wait();
                }
                Ok(())
            })
            .unwrap()
    });
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while entered.load(Ordering::Acquire) == 0 {
        assert!(std::time::Instant::now() < deadline);
        std::thread::yield_now();
    }
    assert_eq!(budget.payload_used(), 256);
    assert!(matches!(store.pin(block, &mut okay), Err(Error::Budget)));
    barrier.wait();
    let first = worker.join().unwrap();
    let second = store.pin(block, &mut okay).unwrap();
    assert!(Arc::ptr_eq(&first.bytes.0, &second.bytes.0));
    assert_eq!(budget.payload_used(), 256);
    drop(first);
    drop(second);
    assert_eq!(budget.payload_used(), 0);
}

#[test]
fn missing_backing_is_an_error_even_when_an_existing_pin_remains_valid() {
    let fixture = Fixture::new();
    let budget = ResidentBudget::new(1024).unwrap();
    let store = fixture.store(&budget);
    let block = store
        .put(TileSpec::new(8, 8, 8).unwrap(), &[71; 256], &mut okay)
        .unwrap();
    let pin = store.pin(block, &mut okay).unwrap();
    fs::rename(&store.0.path, fixture.base.join("retained-pack")).unwrap();
    assert!(store.pin(block, &mut okay).is_err());
    assert_eq!(pin.bytes(), [71; 256]);
    assert_eq!(budget.payload_used(), 256);
    drop(pin);
    assert_eq!(budget.payload_used(), 0);
}

#[test]
fn foreign_directory_replacement_is_never_used_as_a_write_target() {
    let fixture = Fixture::new();
    let budget = ResidentBudget::new(1024).unwrap();
    let store = fixture.store(&budget);
    let spec = TileSpec::new(1, 1, 8).unwrap();
    let block = store.put(spec, &[9; 4], &mut okay).unwrap();
    let original = owned_bytes(&store);
    let replacement = fixture.base.join("retained-source-directory");
    let renamed = fs::rename(fixture.root(), &replacement);
    #[cfg(windows)]
    if let Err(error) = &renamed {
        // Windows may deny renaming a directory containing the held locked file.
        // This native prevention is stronger than detecting a completed replacement.
        assert!(matches!(error.raw_os_error(), Some(5 | 32)));
        assert!(!replacement.exists());
        assert_eq!(owned_bytes(&store), original);
        assert_eq!(store.pin(block, &mut okay).unwrap().bytes(), [9; 4]);
        return;
    }
    renamed.unwrap();
    fs::create_dir(fixture.root()).unwrap();
    fs::write(
        fixture.root().join("blocks.pbt"),
        b"foreign directory contents",
    )
    .unwrap();
    assert!(store.pin(block, &mut okay).is_err());
    assert!(store.put(spec, &[19; 4], &mut okay).is_err());
    assert_eq!(owned_bytes(&store), original);
    drop(store);
    assert_eq!(
        fs::read(fixture.root().join("blocks.pbt")).unwrap(),
        b"foreign directory contents"
    );
}

#[test]
fn corrupt_checkpoint_selection_never_changes_retained_bytes() {
    let fixture = Fixture::new();
    let budget = ResidentBudget::new(1024).unwrap();
    let store = fixture.store(&budget);
    let block = store
        .put(TileSpec::new(1, 1, 8).unwrap(), &[19; 4], &mut okay)
        .unwrap();
    let snapshot = store
        .commit([9; 16], 17, None, b"checkpoint", &[block], &mut okay)
        .unwrap();
    let selected = snapshot.reference();
    let original = owned_bytes(&store);
    drop(snapshot);
    drop(store);
    for field in 0..5 {
        let mut wrong = selected;
        match field {
            0 => wrong.store[0] ^= 1,
            1 => wrong.digest[0] ^= 1,
            2 => wrong.source[0] ^= 1,
            3 => wrong.offset += 1,
            _ => wrong.end -= 1,
        }
        assert!(
            Store::recover(&fixture.root(), wrong, budget.clone(), limits(), &mut okay).is_err()
        );
        assert_eq!(
            fs::read(fixture.root().join("blocks.pbt")).unwrap(),
            original
        );
    }
    let recovered = Store::recover(&fixture.root(), selected, budget, limits(), &mut okay).unwrap();
    assert_eq!(
        recovered.store().pin(block, &mut okay).unwrap().bytes(),
        [19; 4]
    );
}

#[test]
fn held_owner_selects_old_checkpoint_without_advancing_head_or_bypassing_guards() {
    let fixture = Fixture::new();
    let budget = ResidentBudget::new(1024).unwrap();
    let store = fixture.store(&budget);
    assert_ne!(store.identity(), [0; 16]);
    let block = store
        .put(TileSpec::new(1, 1, 16).unwrap(), &[3; 8], &mut okay)
        .unwrap();
    let old = store
        .commit([9; 16], 0, None, b"old", &[block], &mut okay)
        .unwrap()
        .reference();
    let current = store
        .commit([9; 16], 1, Some(old), b"current", &[block], &mut okay)
        .unwrap()
        .reference();
    let selected = store.snapshot(old, &mut okay).unwrap();
    assert_eq!(&*selected.metadata(&mut okay).unwrap(), b"old");
    assert_eq!(store.state().unwrap().head, Some(current));
    assert_eq!(
        selected.store().pin(block, &mut okay).unwrap().bytes(),
        [3; 8]
    );
    for field in 0..6 {
        let mut forged = old;
        match field {
            0 => forged.store[0] ^= 1,
            1 => forged.digest[0] ^= 1,
            2 => forged.offset += 1,
            3 => forged.end += 1,
            4 => forged.source[0] ^= 1,
            _ => forged.revision += 1,
        }
        assert!(store.snapshot(forged, &mut okay).is_err());
    }
    assert!(matches!(
        store.snapshot(old, &mut || Err(Error::Cancelled)),
        Err(Error::Cancelled)
    ));
    let writer = store.0.access.write().unwrap();
    assert!(matches!(store.snapshot(old, &mut okay), Err(Error::Busy)));
    drop(writer);
    assert_eq!(store.state().unwrap().head, Some(current));
    assert_eq!(budget.payload_used(), 0);
}

#[test]
fn embedded_manifest_bytes_are_not_a_selected_record_boundary() {
    let fixture = Fixture::new();
    let store = fixture.store(&ResidentBudget::new(1024).unwrap());
    let mut payload = [0; 256];
    let mut prefix = [0; MANIFEST_HEADER];
    prefix[..16].copy_from_slice(&[7; 16]);
    let digest = digest_parts(b"CMIT", [&prefix[..]], &mut okay).unwrap();
    payload[..4].copy_from_slice(b"CMIT");
    payload[4..12].copy_from_slice(&(MANIFEST_HEADER as u64).to_be_bytes());
    payload[12..44].copy_from_slice(&digest);
    payload[44..108].copy_from_slice(&prefix);
    payload[108..116].copy_from_slice(SEAL);
    let block = store
        .put(TileSpec::new(8, 8, 8).unwrap(), &payload, &mut okay)
        .unwrap();
    store
        .commit([7; 16], 1, None, b"real", &[block], &mut okay)
        .unwrap();
    let offset = block.offset + RECORD_HEADER + 8;
    let forged = CommitRef {
        store: store.identity(),
        digest,
        offset,
        end: offset + 116,
        source: [7; 16],
        revision: 0,
    };
    assert!(matches!(
        store.snapshot(forged, &mut okay),
        Err(Error::Corrupt)
    ));
}

#[test]
fn derived_reference_identity_includes_store_spec_digest_and_offset() {
    let fixture = Fixture::new();
    let store = fixture.store(&ResidentBudget::new(1024).unwrap());
    let block = store
        .put(TileSpec::new(1, 1, 8).unwrap(), &[7; 4], &mut okay)
        .unwrap();
    assert_eq!(block.fingerprint(), block.fingerprint());
    for field in 0..4 {
        let mut other = block;
        match field {
            0 => other.store[0] ^= 1,
            1 => other.spec.depth = 16,
            2 => other.digest[0] ^= 1,
            _ => other.offset += 1,
        }
        assert_ne!(block.fingerprint(), other.fingerprint());
        assert!(store.pin(other, &mut okay).is_err());
    }
}

#[test]
fn persistent_disk_quota_is_shared_and_recovered_files_are_not_double_charged() {
    let fixture = Fixture::new();
    let ram = ResidentBudget::new(1024).unwrap();
    let disk = DiskBudget::new(700, 4).unwrap();
    let first =
        Store::create_with_disk_budget(&fixture.root(), ram.clone(), disk.clone(), limits())
            .unwrap();
    let other_root = fixture.base.join("other");
    let other =
        Store::create_with_disk_budget(&other_root, ram.clone(), disk.clone(), limits()).unwrap();
    assert!(first.uses_disk_budget(&disk));
    let spec = TileSpec::new(8, 8, 8).unwrap();
    let block = first.put(spec, &[1; 256], &mut okay).unwrap();
    let snapshot = first
        .commit([1; 16], 0, None, b"a", &[block], &mut okay)
        .unwrap();
    let selected = snapshot.reference();
    let charged = disk.used().unwrap();
    assert_eq!(
        charged,
        first.0.file.metadata().unwrap().len() + other.0.file.metadata().unwrap().len()
    );
    assert_eq!(first.put(spec, &[1; 256], &mut okay).unwrap(), block);
    assert_eq!(disk.used().unwrap(), charged);
    assert!(matches!(
        other.put(spec, &[2; 256], &mut okay),
        Err(Error::Budget)
    ));
    assert_eq!(other.0.file.metadata().unwrap().len(), FILE_HEADER);
    drop(snapshot);
    drop(first);
    assert_eq!(disk.used().unwrap(), charged);
    let recovered = Store::recover_with_disk_budget(
        &fixture.root(),
        selected,
        ram.clone(),
        disk.clone(),
        limits(),
        &mut okay,
    )
    .unwrap();
    assert_eq!(disk.used().unwrap(), charged);
    assert_eq!(
        recovered.store().pin(block, &mut okay).unwrap().bytes(),
        [1; 256]
    );
    drop(recovered);
    drop(other);
    let inventory = DiskBudget::new(700, 4).unwrap();
    let other_file = File::open(other_root.join("blocks.pbt")).unwrap();
    let other_ticket = inventory
        .retain_file(&other_root.join("blocks.pbt"), &other_file)
        .unwrap();
    drop(other_ticket);
    drop(other_file);
    let recovered = Store::recover_with_disk_budget(
        &fixture.root(),
        selected,
        ram,
        inventory.clone(),
        limits(),
        &mut okay,
    )
    .unwrap();
    assert_eq!(inventory.used().unwrap(), charged);
    drop(recovered);
    assert_eq!(inventory.used().unwrap(), charged);
}

#[test]
fn failed_partial_source_append_keeps_its_conservative_disk_charge() {
    let fixture = Fixture::new();
    let disk = DiskBudget::new(4096, 2).unwrap();
    let store = Store::create_with_disk_budget(
        &fixture.root(),
        ResidentBudget::new(1024).unwrap(),
        disk.clone(),
        limits(),
    )
    .unwrap();
    let before = disk.used().unwrap();
    assert!(matches!(
        store.put(TileSpec::new(8, 8, 8).unwrap(), &[9; 256], &mut || {
            if store.0.file.metadata()?.len() > FILE_HEADER {
                Err(Error::Cancelled)
            } else {
                Ok(())
            }
        }),
        Err(Error::Cancelled)
    ));
    assert!(disk.used().unwrap() > before);
    assert!(disk.used().unwrap() >= store.0.file.metadata().unwrap().len());
    let charged = disk.used().unwrap();
    drop(store);
    assert_eq!(disk.used().unwrap(), charged);
}

#[test]
fn explicit_disk_inventory_rechecks_every_file_after_the_last_callback() {
    let fixture = Fixture::new();
    let store = fixture.store(&ResidentBudget::new(1024).unwrap());
    let paths = [(&store.0.path as &Path, &store.0.file)];
    let budget = DiskBudget::from_inventory(1024, 2, &paths, &mut okay).unwrap();
    assert_eq!(budget.used().unwrap(), FILE_HEADER);
    let mut checks = 0;
    let result = DiskBudget::from_inventory(1024, 2, &paths, &mut || {
        checks += 1;
        if checks == 2 {
            write_all_at(&store.0.file, FILE_HEADER, b"late orphan bytes")?;
        }
        Ok(())
    });
    assert!(matches!(result, Err(Error::Changed)));
    assert_eq!(store.0.file.metadata().unwrap().len(), FILE_HEADER + 17);
}

#[test]
fn quota_refusal_creates_no_uninventoried_source_directory() {
    let fixture = Fixture::new();
    let disk = DiskBudget::new(FILE_HEADER - 1, 1).unwrap();
    assert!(matches!(
        Store::create_with_disk_budget(
            &fixture.root(),
            ResidentBudget::new(1024).unwrap(),
            disk.clone(),
            limits()
        ),
        Err(Error::Budget)
    ));
    assert!(!fixture.root().exists());
    assert_eq!(disk.used().unwrap(), 0);
}

#[test]
fn held_checkpoint_selection_rechecks_late_source_changes() {
    let fixture = Fixture::new();
    let store = fixture.store(&ResidentBudget::new(1024).unwrap());
    let selected = store
        .commit([9; 16], 0, None, b"retained", &[], &mut okay)
        .unwrap()
        .reference();
    let before = store.0.file.metadata().unwrap().len();
    let mut changed = false;
    let result = store.snapshot(selected, &mut || {
        if !changed {
            write_all_at(&store.0.file, before, b"foreign tail")?;
            changed = true;
        }
        Ok(())
    });
    assert!(matches!(result, Err(Error::Changed)));
    assert_eq!(store.state().unwrap().head, Some(selected));
    assert_eq!(store.0.file.metadata().unwrap().len(), before + 12);
}

#[test]
#[ignore = "Opt-in actual dense backing component; needs 8 GiB private disk, not full 16000 editor acceptance"]
fn dense_three_layer_native16_backing_crosses_four_gib_with_bounded_pins() {
    let started = std::time::Instant::now();
    let fixture = Fixture::new();
    let budget = ResidentBudget::new(8 * 1024 * 1024).unwrap();
    let mut admitted = limits();
    admitted.disk_bytes = 8 * 1024 * 1024 * 1024;
    admitted.max_blocks = 16384;
    let disk = DiskBudget::new(admitted.disk_bytes, 4).unwrap();
    let store =
        Store::create_with_disk_budget(&fixture.root(), budget.clone(), disk.clone(), admitted)
            .unwrap();
    assert!(store.uses_disk_budget(&disk));
    let spec = TileSpec::new(256, 256, 16).unwrap();
    let tile_count = 3 * 63 * 63;
    let mut source = vec![0; spec.bytes()];
    let mut refs = Vec::with_capacity(tile_count);
    let fill = |bytes: &mut [u8], tile: usize| {
        let mut random = (tile as u32).wrapping_add(0x98765432);
        for word in bytes.chunks_exact_mut(2) {
            random = random.wrapping_mul(1664525).wrapping_add(1013904223);
            word.copy_from_slice(&((random >> 16) as u16).to_be_bytes());
        }
    };
    for tile in 0..tile_count {
        fill(&mut source, tile);
        let reference = store.put(spec, &source, &mut okay).unwrap();
        let pin = store.pin(reference, &mut okay).unwrap();
        assert_eq!(pin.bytes(), source);
        assert_eq!(budget.payload_used(), spec.bytes());
        drop(pin);
        assert_eq!(budget.payload_used(), 0);
        refs.push(reference);
        if (tile + 1) % (63 * 63) == 0 {
            println!(
                "dense backing component layer={} encoded_bytes={} resident_pin_bytes={}",
                (tile + 1) / (63 * 63),
                store.0.file.metadata().unwrap().len(),
                budget.payload_used()
            );
        }
    }
    assert!(refs.last().unwrap().offset > u32::MAX as u64);
    let snapshot = store
        .commit(
            [5; 16],
            0,
            None,
            b"{\"width\":16000,\"height\":16000,\"depth\":16,\"layers\":3}",
            &refs,
            &mut okay,
        )
        .unwrap();
    let selected = snapshot.reference();
    let physical = store.0.file.metadata().unwrap().len();
    assert!(physical > 6_000_000_000);
    assert_eq!(disk.used().unwrap(), physical);
    let mut context = Context::new(&SHA256);
    let mut buffer = [0; CHUNK];
    let mut offset = 0;
    while offset < physical {
        let count = (physical - offset).min(CHUNK as u64) as usize;
        read_exact_at(&store.0.file, offset, &mut buffer[..count]).unwrap();
        context.update(&buffer[..count]);
        offset += count as u64;
    }
    let file_sha256 = context
        .finish()
        .as_ref()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    drop(snapshot);
    drop(store);
    assert_eq!(disk.used().unwrap(), physical);
    let recovered = Store::recover_with_disk_budget(
        &fixture.root(),
        selected,
        budget.clone(),
        disk.clone(),
        admitted,
        &mut okay,
    )
    .unwrap();
    assert_eq!(disk.used().unwrap(), physical);
    let mut count = 0;
    recovered
        .visit_blocks(&mut okay, |reference| {
            assert_eq!(reference, refs[count]);
            count += 1;
            Ok(())
        })
        .unwrap();
    assert_eq!(count, tile_count);
    for tile in [0, 63 * 63 - 1, 2 * 63 * 63 - 1, tile_count - 1] {
        fill(&mut source, tile);
        let pin = recovered.store.pin(refs[tile], &mut okay).unwrap();
        assert_eq!(pin.bytes(), source);
        assert_eq!(budget.payload_used(), spec.bytes());
        drop(pin);
    }
    assert_eq!(budget.payload_used(), 0);
    let checkpoint_sha256 = selected
        .digest
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    drop(recovered);
    assert_eq!(disk.used().unwrap(), physical);
    println!("dense backing component: {tile_count} full native16 tiles, {physical} encoded file bytes (all payload bytes written, allocated disk clusters not measured), canonical BE readback, resident pin payload <= {} bytes, persistent_reserved_bytes={}, retained after owner drop, elapsed_ms={}, file_sha256={file_sha256}, checkpoint_sha256={checkpoint_sha256}; no total RAM/editor/PSB/history qualification",spec.bytes(),disk.used().unwrap(),started.elapsed().as_millis());
}
