//! Shared logical-file admission. Dropping a ticket never frees disk capacity.
use super::{index::Index, Error, ResidentBudget, Result};
use std::{
    collections::BTreeMap,
    fs::File,
    path::Path,
    sync::{Arc, Mutex},
};

type Identity = (u64, u64);
#[path = "disk/external.rs"]
mod external;
pub(crate) use external::{ExternalFact, PreparedSettlement, PublicationTicket};

#[derive(Clone)]
pub struct DiskBudget(Arc<Inner>);
struct Inner {
    limit: u64,
    maximum: usize,
    state: Mutex<State>,
    resident: Option<ResidentBudget>,
}
struct State {
    used: u64,
    external: u64,
    active_external: usize,
    next: u64,
    files: BTreeMap<u64, Entry>,
    coexistence: Index<[u8; 32], ExternalEntry>,
    known_external: Index<Identity, ExternalFact>,
    publication_targets: Index<[u8; 32], external::Target>,
}
#[derive(Clone)]
struct ExternalEntry {
    key: u64,
    bytes: u64,
    settled: bool,
}
struct Entry {
    identity: Option<Identity>,
    bytes: u64,
}

/// A persistent charge. It has deliberately no refund-on-drop behavior.
pub struct DiskReservation {
    budget: DiskBudget,
    key: u64,
}

/// Operation admission for bytes that can leave the source namespace. Dropping
/// it retains the debt; it never claims that a saved destination was unlinked.
pub(crate) struct CoexistenceTicket {
    budget: DiskBudget,
    intent: [u8; 32],
    key: u64,
    bytes: u64,
}

/// Implemented only by the caller's checked, held-identity publication or
/// retirement receipt. An intent hash or a deserialized path is not a receipt.
pub(crate) trait CoexistenceSettlement {
    fn validate(&self, intent: [u8; 32], bytes: u64) -> Result<()>;
}

impl DiskBudget {
    /// Rebuild charges from the actual owner's explicit held inventory. Every
    /// observation is repeated after the last caller check before publication.
    /// This counts bytes only; it cannot authenticate a source or acquire a lock.
    pub fn from_inventory(
        limit: u64,
        maximum_files: usize,
        files: &[(&Path, &File)],
        check: &mut dyn FnMut() -> Result<()>,
    ) -> Result<Self> {
        if files.len() > maximum_files {
            return Err(Error::Budget);
        }
        let budget = Self::new(limit, maximum_files)?;
        let mut observed = Vec::new();
        observed
            .try_reserve_exact(files.len())
            .map_err(|_| Error::Budget)?;
        for (path, file) in files {
            check()?;
            let before = super::store::stamp(file)?;
            budget.retain_file(path, file)?;
            observed.push(before);
        }
        check()?;
        // No external callbacks run after starting this final inventory pass.
        for ((path, file), before) in files.iter().zip(observed) {
            super::store::validate_ancestors(path.parent().ok_or(Error::Changed)?)?;
            super::store::regular(path)?;
            let named =
                crate::file_safety::open_native_identity(path).map_err(|_| Error::Changed)?;
            if super::store::stamp(file)? != before
                || super::store::links(file)? != 1
                || crate::file_safety::identity(&named).map_err(|_| Error::Changed)?
                    != crate::file_safety::identity(file).map_err(|_| Error::Changed)?
            {
                return Err(Error::Changed);
            }
        }
        Ok(budget)
    }
    /// Only an empty, independently owned namespace may start with no inventory.
    /// Existing source, history, backup, and orphan files must first be retained
    /// through `retain_file` under the real instance/profile admission owner.
    /// This ledger is quota accounting; it never grants filesystem authority.
    pub fn new(limit: u64, maximum_files: usize) -> Result<Self> {
        Self::create(limit, maximum_files, None)
    }
    /// Shared source-operation ledgers retain charged settled-intent metadata
    /// until teardown. The payload pool must be the actual source runtime pool.
    pub fn with_resident(
        limit: u64,
        maximum_files: usize,
        resident: ResidentBudget,
    ) -> Result<Self> {
        Self::create(limit, maximum_files, Some(resident))
    }
    fn create(limit: u64, maximum_files: usize, resident: Option<ResidentBudget>) -> Result<Self> {
        if limit == 0 || maximum_files == 0 || maximum_files > 65536 {
            return Err(Error::Budget);
        }
        Ok(Self(Arc::new(Inner {
            limit,
            maximum: maximum_files,
            resident,
            state: Mutex::new(State {
                used: 0,
                external: 0,
                active_external: 0,
                next: 0,
                files: BTreeMap::new(),
                coexistence: Index::new(),
                known_external: Index::new(),
                publication_targets: Index::new(),
            }),
        })))
    }
    pub fn used(&self) -> Result<u64> {
        Ok(self.0.state.lock().map_err(|_| Error::Interrupted)?.used)
    }
    /// Persistent source bytes plus retained external operation admission.
    /// This is an admission counter, not a physical free-space observation.
    pub fn admitted(&self) -> Result<u64> {
        let state = self.0.state.lock().map_err(|_| Error::Interrupted)?;
        state.used.checked_add(state.external).ok_or(Error::Corrupt)
    }
    /// Observation for destination refusal only, never filesystem authority.
    pub(crate) fn is_internal_identity(&self, identity: (u64, u64)) -> Result<bool> {
        let state = self.0.state.lock().map_err(|_| Error::Interrupted)?;
        Ok(state
            .files
            .values()
            .any(|entry| entry.identity == Some(identity)))
    }

    /// The actual caller must durably publish its authenticated orphan intent
    /// BEFORE creating any external staging file covered by this ticket.
    pub(crate) fn reserve_coexistence(
        &self,
        intent: [u8; 32],
        bytes: u64,
    ) -> Result<CoexistenceTicket> {
        self.coexistence(intent, bytes, false)
    }

    /// Reconstruct retained debt from the actual owner's authenticated journal.
    /// Re-observing the same intent is idempotent; conflicting bytes fail closed.
    pub(crate) fn restore_coexistence(
        &self,
        intent: [u8; 32],
        bytes: u64,
    ) -> Result<CoexistenceTicket> {
        self.coexistence(intent, bytes, true)
    }

    fn coexistence(
        &self,
        intent: [u8; 32],
        bytes: u64,
        restore: bool,
    ) -> Result<CoexistenceTicket> {
        let mut state = self.0.state.lock().map_err(|_| Error::Interrupted)?;
        if let Some(entry) = state.coexistence.get(&intent) {
            if entry.settled || !restore || entry.bytes != bytes {
                return Err(Error::Changed);
            }
            return Ok(CoexistenceTicket {
                budget: self.clone(),
                intent,
                key: entry.key,
                bytes,
            });
        }
        if state.files.len() + state.known_external.len() + state.active_external >= self.0.maximum
        {
            return Err(Error::Budget);
        }
        let external = state.external.checked_add(bytes).ok_or(Error::Budget)?;
        state
            .used
            .checked_add(external)
            .filter(|v| *v <= self.0.limit)
            .ok_or(Error::Budget)?;
        let key = state.next;
        state
            .coexistence
            .reserve_one(self.0.resident.as_ref().ok_or(Error::Budget)?, 65536)?;
        state.next = key.checked_add(1).ok_or(Error::Budget)?;
        state.coexistence.insert_reserved(
            intent,
            ExternalEntry {
                key,
                bytes,
                settled: false,
            },
        )?;
        state.external = external;
        state.active_external += 1;
        Ok(CoexistenceTicket {
            budget: self.clone(),
            intent,
            key,
            bytes,
        })
    }
    pub fn limit(&self) -> u64 {
        self.0.limit
    }
    pub fn same_pool(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }

    pub fn reserve(&self, bytes: u64) -> Result<DiskReservation> {
        let mut state = self.0.state.lock().map_err(|_| Error::Interrupted)?;
        let used = state
            .used
            .checked_add(bytes)
            .filter(|v| {
                v.checked_add(state.external)
                    .is_some_and(|n| n <= self.0.limit)
            })
            .ok_or(Error::Budget)?;
        if state.files.len() + state.known_external.len() + state.active_external >= self.0.maximum
        {
            return Err(Error::Budget);
        }
        let key = state.next;
        state.next = key.checked_add(1).ok_or(Error::Budget)?;
        state.files.insert(
            key,
            Entry {
                identity: None,
                bytes,
            },
        );
        state.used = used;
        Ok(DiskReservation {
            budget: self.clone(),
            key,
        })
    }

    /// Count an explicitly inventoried held file once, including incomplete or
    /// unselected files. A directory scan alone cannot make a source admissible.
    pub fn retain_file(&self, path: &Path, file: &File) -> Result<DiskReservation> {
        super::store::validate_ancestors(path.parent().ok_or(Error::Changed)?)?;
        super::store::regular(path)?;
        let identity = crate::file_safety::identity(file).map_err(|_| Error::Changed)?;
        let named = crate::file_safety::open_native_identity(path).map_err(|_| Error::Changed)?;
        let before = super::store::stamp(file)?;
        if crate::file_safety::identity(&named).map_err(|_| Error::Changed)? != identity
            || super::store::links(file)? != 1
        {
            return Err(Error::Changed);
        }
        let mut state = self.0.state.lock().map_err(|_| Error::Interrupted)?;
        if state.known_external.get(&identity).is_some() {
            return Err(Error::Changed);
        }
        let existing = state
            .files
            .iter()
            .find_map(|(key, entry)| (entry.identity == Some(identity)).then_some(*key));
        let reservation = if let Some(key) = existing {
            if state.files[&key].bytes < before.bytes {
                return Err(Error::Changed);
            }
            DiskReservation {
                budget: self.clone(),
                key,
            }
        } else {
            if state.files.len() + state.active_external >= self.0.maximum {
                return Err(Error::Budget);
            }
            let used = state
                .used
                .checked_add(before.bytes)
                .filter(|v| {
                    v.checked_add(state.external)
                        .is_some_and(|n| n <= self.0.limit)
                })
                .ok_or(Error::Budget)?;
            let key = state.next;
            state.next = key.checked_add(1).ok_or(Error::Budget)?;
            state.files.insert(
                key,
                Entry {
                    identity: Some(identity),
                    bytes: before.bytes,
                },
            );
            state.used = used;
            DiskReservation {
                budget: self.clone(),
                key,
            }
        };
        drop(state);
        // A failed final observation retains its conservative charge.
        let after = crate::file_safety::open_native_identity(path).map_err(|_| Error::Changed)?;
        if super::store::stamp(file)? != before
            || super::store::links(file)? != 1
            || crate::file_safety::identity(&after).map_err(|_| Error::Changed)? != identity
        {
            return Err(Error::Changed);
        }
        Ok(reservation)
    }
}

impl DiskReservation {
    pub fn bytes(&self) -> Result<u64> {
        self.budget
            .0
            .state
            .lock()
            .map_err(|_| Error::Interrupted)?
            .files
            .get(&self.key)
            .map(|entry| entry.bytes)
            .ok_or(Error::Changed)
    }
    pub(crate) fn bind(&self, file: &File) -> Result<()> {
        let identity = crate::file_safety::identity(file).map_err(|_| Error::Changed)?;
        if super::store::links(file)? != 1 {
            return Err(Error::Changed);
        }
        let length = file.metadata()?.len();
        let mut state = self.budget.0.state.lock().map_err(|_| Error::Interrupted)?;
        if state.known_external.get(&identity).is_some()
            || state
                .files
                .iter()
                .any(|(key, entry)| *key != self.key && entry.identity == Some(identity))
        {
            return Err(Error::Changed);
        }
        let entry = state.files.get_mut(&self.key).ok_or(Error::Changed)?;
        if entry.identity.is_some_and(|old| old != identity) || length > entry.bytes {
            return Err(Error::Changed);
        }
        entry.identity = Some(identity);
        Ok(())
    }
    pub(super) fn uses_budget(&self, budget: &DiskBudget) -> bool {
        self.budget.same_pool(budget)
    }
    pub(super) fn budget(&self) -> DiskBudget {
        self.budget.clone()
    }
    pub(crate) fn grow(&self, extra: u64) -> Result<()> {
        let mut state = self.budget.0.state.lock().map_err(|_| Error::Interrupted)?;
        let used = state
            .used
            .checked_add(extra)
            .filter(|v| {
                v.checked_add(state.external)
                    .is_some_and(|n| n <= self.budget.0.limit)
            })
            .ok_or(Error::Budget)?;
        let entry = state.files.get_mut(&self.key).ok_or(Error::Changed)?;
        entry.bytes = entry.bytes.checked_add(extra).ok_or(Error::Budget)?;
        state.used = used;
        Ok(())
    }
    /// Called only after the source owner has proved exclusive retirement and
    /// closed its last held file; no public arbitrary refund operation exists.
    pub(super) fn release_retired(self, identity: Identity) -> Result<()> {
        let mut state = self.budget.0.state.lock().map_err(|_| Error::Interrupted)?;
        let entry = state.files.get(&self.key).ok_or(Error::Changed)?;
        if entry.identity != Some(identity) {
            return Err(Error::Changed);
        }
        let bytes = entry.bytes;
        state.files.remove(&self.key);
        state.used = state.used.checked_sub(bytes).ok_or(Error::Corrupt)?;
        Ok(())
    }
}

impl CoexistenceTicket {
    pub(crate) fn settle(self, proof: &impl CoexistenceSettlement) -> Result<()> {
        // Native receipt validation can perform I/O and re-enter this budget.
        // It must never run while the accounting mutex is held.
        proof.validate(self.intent, self.bytes)?;
        let mut state = self.budget.0.state.lock().map_err(|_| Error::Interrupted)?;
        let entry = state.coexistence.get(&self.intent).ok_or(Error::Changed)?;
        if entry.settled || entry.key != self.key || entry.bytes != self.bytes {
            return Err(Error::Changed);
        }
        let external = state
            .external
            .checked_sub(self.bytes)
            .ok_or(Error::Corrupt)?;
        let active_external = state.active_external.checked_sub(1).ok_or(Error::Corrupt)?;
        state.coexistence.insert_reserved(
            self.intent,
            ExternalEntry {
                key: self.key,
                bytes: self.bytes,
                settled: true,
            },
        )?;
        state.external = external;
        state.active_external = active_external;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Receipt(DiskBudget, [u8; 32], u64, bool);
    impl CoexistenceSettlement for Receipt {
        fn validate(&self, intent: [u8; 32], bytes: u64) -> Result<()> {
            // Re-entry proves no accounting mutex is held during validation.
            self.0.admitted()?;
            if self.3 && self.1 == intent && self.2 == bytes {
                Ok(())
            } else {
                Err(Error::Changed)
            }
        }
    }

    #[test]
    fn coexistence_blocks_growth_and_keeps_failed_or_dropped_debt() {
        let budget =
            DiskBudget::with_resident(1024, 4, ResidentBudget::new(4096).unwrap()).unwrap();
        let source = budget.reserve(128).unwrap();
        let intent = [7; 32];
        let ticket = budget.reserve_coexistence(intent, 768).unwrap();
        assert_eq!(budget.used().unwrap(), 128);
        assert_eq!(budget.admitted().unwrap(), 896);
        assert!(matches!(source.grow(129), Err(Error::Budget)));
        assert!(matches!(budget.reserve(129), Err(Error::Budget)));
        assert!(matches!(
            budget.reserve_coexistence([8; 32], u64::MAX),
            Err(Error::Budget)
        ));
        assert!(matches!(
            ticket.settle(&Receipt(budget.clone(), intent, 768, false)),
            Err(Error::Changed)
        ));
        assert_eq!(budget.admitted().unwrap(), 896);
        drop(budget.restore_coexistence(intent, 768).unwrap());
        assert_eq!(budget.admitted().unwrap(), 896);
        let ticket = budget.restore_coexistence(intent, 768).unwrap();
        ticket
            .settle(&Receipt(budget.clone(), intent, 768, true))
            .unwrap();
        assert_eq!(budget.admitted().unwrap(), 128);
        source.grow(896).unwrap();
    }

    #[test]
    fn coexistence_reconstruction_is_exact_and_stale_tickets_cannot_settle_new_debt() {
        let budget =
            DiskBudget::with_resident(1024, 1, ResidentBudget::new(4096).unwrap()).unwrap();
        let intent = [9; 32];
        let ticket = budget.restore_coexistence(intent, 512).unwrap();
        let stale = budget.restore_coexistence(intent, 512).unwrap();
        assert!(matches!(
            budget.restore_coexistence(intent, 511),
            Err(Error::Changed)
        ));
        assert!(matches!(
            budget.reserve_coexistence(intent, 512),
            Err(Error::Changed)
        ));
        assert!(matches!(budget.reserve(0), Err(Error::Budget)));
        let proof = Receipt(budget.clone(), intent, 512, true);
        ticket.settle(&proof).unwrap();
        assert!(matches!(
            budget.reserve_coexistence(intent, 512),
            Err(Error::Changed)
        ));
        assert!(matches!(
            budget.restore_coexistence(intent, 512),
            Err(Error::Changed)
        ));
        let replacement = budget.reserve_coexistence([10; 32], 512).unwrap();
        assert!(matches!(stale.settle(&proof), Err(Error::Changed)));
        assert_eq!(budget.admitted().unwrap(), 512);
        replacement
            .settle(&Receipt(budget.clone(), [10; 32], 512, true))
            .unwrap();
        assert_eq!(budget.admitted().unwrap(), 0);
    }

    #[test]
    fn coexistence_metadata_is_admitted_before_growth_and_tombstones_keep_the_charge() {
        let missing = DiskBudget::new(1024, 2).unwrap();
        assert!(matches!(
            missing.reserve_coexistence([1; 32], 1),
            Err(Error::Budget)
        ));
        let tiny = ResidentBudget::new(1).unwrap();
        let refused = DiskBudget::with_resident(1024, 2, tiny.clone()).unwrap();
        assert!(matches!(
            refused.reserve_coexistence([1; 32], 1),
            Err(Error::Budget)
        ));
        assert_eq!(tiny.used(), 0);
        assert_eq!(refused.admitted().unwrap(), 0);
        let resident = ResidentBudget::new(4096).unwrap();
        let budget = DiskBudget::with_resident(1024, 2, resident.clone()).unwrap();
        let ticket = budget.reserve_coexistence([1; 32], 512).unwrap();
        let metadata = resident.used();
        assert!(metadata >= std::mem::size_of::<([u8; 32], ExternalEntry)>());
        ticket
            .settle(&Receipt(budget.clone(), [1; 32], 512, true))
            .unwrap();
        assert_eq!(budget.admitted().unwrap(), 0);
        assert_eq!(resident.used(), metadata);
        assert!(matches!(
            budget.reserve_coexistence([1; 32], 512),
            Err(Error::Changed)
        ));
        drop(budget);
        assert_eq!(resident.used(), 0);
    }
    #[test]
    fn concurrent_source_and_coexistence_admission_cannot_overcommit() {
        use std::sync::Barrier;
        for _ in 0..64 {
            let budget =
                DiskBudget::with_resident(1024, 4, ResidentBudget::new(4096).unwrap()).unwrap();
            let source = budget.reserve(0).unwrap();
            let start = Arc::new(Barrier::new(3));
            let a = start.clone();
            let b = start.clone();
            let external = budget.clone();
            let source = std::thread::spawn(move || {
                a.wait();
                source.grow(768)
            });
            let external = std::thread::spawn(move || {
                b.wait();
                external.reserve_coexistence([11; 32], 768)
            });
            start.wait();
            let source = source.join().unwrap();
            let external = external.join().unwrap();
            assert_ne!(source.is_ok(), external.is_ok());
            assert_eq!(budget.admitted().unwrap(), 768);
        }
    }

    #[test]
    fn failed_and_dropped_reservations_stay_charged_and_shared() {
        let budget = DiskBudget::new(1024, 2).unwrap();
        let ticket = budget.reserve(768).unwrap();
        assert!(budget.same_pool(&budget.clone()));
        assert!(!budget.same_pool(&DiskBudget::new(1024, 2).unwrap()));
        assert!(matches!(budget.reserve(257), Err(Error::Budget)));
        assert!(matches!(ticket.grow(u64::MAX), Err(Error::Budget)));
        drop(ticket);
        assert_eq!(budget.used().unwrap(), 768);
        drop(budget.reserve(256).unwrap());
        assert_eq!(budget.used().unwrap(), 1024);
        assert!(matches!(budget.reserve(0), Err(Error::Budget)));
    }
}
