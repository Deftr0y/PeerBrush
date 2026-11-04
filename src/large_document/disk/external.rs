//! Global external file debt. These facts/tickets are quota data only; they
//! never grant filesystem write, deletion, publication or recovery authority.
use super::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ExternalFact {
    pub(crate) identity: Identity,
    pub(crate) bytes: u64,
    pub(crate) modified: u128,
}
#[derive(Clone, Copy)]
pub(super) struct Target {
    intent: [u8; 32],
    key: u64,
    basis: [u8; 32],
    replaced: Option<Identity>,
    slots: usize,
    active: bool,
}
/// Dropping this ticket retains the full reserved growth and target lock.
/// Only a separately qualified Root outcome may authorize final transfer.
pub(crate) struct PublicationTicket {
    budget: DiskBudget,
    target: [u8; 32],
    intent: [u8; 32],
    key: u64,
    growth: u64,
}
pub(crate) struct PreparedSettlement {
    facts: Vec<ExternalFact>,
    _charge: super::super::Reservation,
    ticket: PublicationTicket,
}
fn validate(facts: &[ExternalFact], maximum: usize) -> Result<()> {
    if facts.len() > maximum || facts.windows(2).any(|w| w[0].identity >= w[1].identity) {
        return Err(Error::Changed);
    }
    Ok(())
}
fn basis(facts: &[ExternalFact], replaced: Option<Identity>) -> [u8; 32] {
    let mut hash = ring::digest::Context::new(&ring::digest::SHA256);
    hash.update(b"PeerBrushExternalDebtBasis1\0");
    hash.update(&(facts.len() as u64).to_be_bytes());
    for fact in facts {
        hash.update(&fact.identity.0.to_be_bytes());
        hash.update(&fact.identity.1.to_be_bytes());
        hash.update(&fact.bytes.to_be_bytes());
        hash.update(&fact.modified.to_be_bytes());
    }
    hash.update(&[u8::from(replaced.is_some())]);
    if let Some((volume, file)) = replaced {
        hash.update(&volume.to_be_bytes());
        hash.update(&file.to_be_bytes());
    }
    hash.finish().as_ref().try_into().unwrap()
}
impl DiskBudget {
    pub(crate) fn reserve_publication(
        &self,
        intent: [u8; 32],
        target: [u8; 32],
        facts: &[ExternalFact],
        replaced: Option<Identity>,
        growth: u64,
    ) -> Result<PublicationTicket> {
        self.publication(intent, target, facts, replaced, growth, false, false)
    }
    /// Read-only journal reconstruction. Repeating the exact pending record is
    /// idempotent; ambiguity never creates a fresh writable publication ticket.
    pub(crate) fn restore_publication(
        &self,
        intent: [u8; 32],
        target: [u8; 32],
        facts: &[ExternalFact],
        replaced: Option<Identity>,
        growth: u64,
    ) -> Result<PublicationTicket> {
        self.publication(intent, target, facts, replaced, growth, true, false)
    }
    /// Fresh-ledger read-only reconstruction. This only ADDS observed debt and a
    /// settled tombstone; it cannot settle/refund an existing pending ticket.
    pub(crate) fn restore_external(
        &self,
        intent: [u8; 32],
        target: [u8; 32],
        facts: &[ExternalFact],
        growth: u64,
    ) -> Result<()> {
        self.publication(intent, target, facts, None, growth, true, true)
            .map(|_| ())
    }
    pub(crate) fn retain_settled_intent(&self, intent: [u8; 32], growth: u64) -> Result<()> {
        if intent == [0; 32] {
            return Err(Error::Changed);
        }
        let mut state = self.0.state.lock().map_err(|_| Error::Interrupted)?;
        if let Some(old) = state.coexistence.get(&intent) {
            return if old.settled && old.bytes == growth {
                Ok(())
            } else {
                Err(Error::Changed)
            };
        }
        state
            .coexistence
            .reserve_one(self.0.resident.as_ref().ok_or(Error::Budget)?, 65536)?;
        let key = state.next;
        let next = key.checked_add(1).ok_or(Error::Budget)?;
        state.coexistence.insert_reserved(
            intent,
            ExternalEntry {
                key,
                bytes: growth,
                settled: true,
            },
        )?;
        state.next = next;
        Ok(())
    }
    fn publication(
        &self,
        intent: [u8; 32],
        target: [u8; 32],
        facts: &[ExternalFact],
        replaced: Option<Identity>,
        growth: u64,
        restore: bool,
        settled: bool,
    ) -> Result<PublicationTicket> {
        validate(facts, self.0.maximum)?;
        if intent == [0; 32]
            || target == [0; 32]
            || replaced.is_some_and(|id| !facts.iter().any(|f| f.identity == id))
        {
            return Err(Error::Changed);
        }
        let basis = basis(facts, replaced);
        let mut state = self.0.state.lock().map_err(|_| Error::Interrupted)?;
        if let Some(old) = state.coexistence.get(&intent) {
            let current = state
                .publication_targets
                .get(&target)
                .ok_or(Error::Changed)?;
            if !restore
                || old.settled != settled
                || old.bytes != growth
                || current.active == settled
                || current.intent != intent
                || current.key != old.key
                || current.basis != basis
                || current.replaced != replaced
            {
                return Err(Error::Changed);
            }
            return Ok(PublicationTicket {
                budget: self.clone(),
                target,
                intent,
                key: old.key,
                growth,
            });
        }
        if state
            .publication_targets
            .get(&target)
            .is_some_and(|entry| entry.active)
        {
            return Err(Error::Busy);
        }
        let mut added = 0u64;
        let mut count = 0usize;
        for fact in facts {
            // An internal source file cannot be relabelled as an external file
            // to bypass its immutable namespace/retirement rules.
            if !(restore && !settled)
                && state
                    .files
                    .values()
                    .any(|entry| entry.identity == Some(fact.identity))
            {
                return Err(Error::Changed);
            }
            // A known pending record can describe an old, now-reused FileId.
            // Read-only reconstruction conservatively charges both domains;
            // no live PublicationIntent is reconstructed, and retirement of an
            // actually internal ID remains forbidden by transfer_checked.
            match state.known_external.get(&fact.identity) {
                Some(old) if old != fact => return Err(Error::Changed),
                Some(_) => (),
                None => {
                    added = added.checked_add(fact.bytes).ok_or(Error::Budget)?;
                    count += 1;
                }
            }
        }
        let slots = if settled {
            0
        } else if replaced.is_some() {
            2
        } else {
            1
        };
        if state
            .files
            .len()
            .checked_add(state.known_external.len())
            .and_then(|n| n.checked_add(count))
            .and_then(|n| n.checked_add(state.active_external))
            .and_then(|n| n.checked_add(slots))
            .is_none_or(|n| n > self.0.maximum)
        {
            return Err(Error::Budget);
        }
        let external = state
            .external
            .checked_add(added)
            .and_then(|n| n.checked_add(if settled { 0 } else { growth }))
            .ok_or(Error::Budget)?;
        state
            .used
            .checked_add(external)
            .filter(|n| *n <= self.0.limit)
            .ok_or(Error::Budget)?;
        let pool = self.0.resident.as_ref().ok_or(Error::Budget)?;
        let known = state
            .known_external
            .len()
            .checked_add(count)
            .and_then(|n| n.checked_add(state.active_external))
            .and_then(|n| n.checked_add(slots))
            .ok_or(Error::Budget)?;
        state
            .known_external
            .reserve_to(known, pool, self.0.maximum)?;
        state.coexistence.reserve_one(pool, 65536)?;
        let targets = state.publication_targets.len()
            + usize::from(state.publication_targets.get(&target).is_none());
        state.publication_targets.reserve_to(targets, pool, 65536)?;
        let key = state.next;
        let next = key.checked_add(1).ok_or(Error::Budget)?;
        for fact in facts {
            state.known_external.insert_reserved(fact.identity, *fact)?;
        }
        state.coexistence.insert_reserved(
            intent,
            ExternalEntry {
                key,
                bytes: growth,
                settled,
            },
        )?;
        state.publication_targets.insert_reserved(
            target,
            Target {
                intent,
                key,
                basis,
                replaced,
                slots,
                active: !settled,
            },
        )?;
        state.next = next;
        state.external = external;
        state.active_external += slots;
        Ok(PublicationTicket {
            budget: self.clone(),
            target,
            intent,
            key,
            growth,
        })
    }
}
impl PublicationTicket {
    pub(crate) fn prepare_settlement(&self, maximum: usize) -> Result<PreparedSettlement> {
        if maximum > self.budget.0.maximum {
            return Err(Error::Budget);
        }
        let pool = self.budget.0.resident.as_ref().ok_or(Error::Budget)?;
        let bytes = maximum
            .checked_mul(2 * std::mem::size_of::<ExternalFact>())
            .and_then(|n| n.checked_add(128))
            .ok_or(Error::Budget)?;
        let charge = pool.reserve_bytes(bytes, 64 * 1024 * 1024)?;
        let mut facts = Vec::new();
        facts
            .try_reserve_exact(maximum)
            .map_err(|_| Error::Budget)?;
        if facts
            .capacity()
            .checked_mul(std::mem::size_of::<ExternalFact>())
            .is_none_or(|n| n > bytes)
        {
            return Err(Error::Budget);
        }
        Ok(PreparedSettlement {
            facts,
            _charge: charge,
            ticket: Self {
                budget: self.budget.clone(),
                target: self.target,
                intent: self.intent,
                key: self.key,
                growth: self.growth,
            },
        })
    }
    pub(crate) fn growth(&self) -> u64 {
        self.growth
    }
    pub(crate) fn intent(&self) -> [u8; 32] {
        self.intent
    }
    pub(crate) fn target(&self) -> [u8; 32] {
        self.target
    }
    pub(crate) fn uses_budget(&self, budget: &DiskBudget) -> bool {
        self.budget.same_pool(budget)
    }

    /// Only the closed filesystem writer can implement the production proof.
    /// Its I/O checks precede BOTH the temporary metadata admission and ledger
    /// mutex; an arbitrary callback, path, digest or decoded frame cannot settle.
    pub(crate) fn settle(
        &self,
        proof: &dyn crate::file_safety::ExternalOutcomeProof,
    ) -> Result<()> {
        proof
            .validate(self.intent, self.target, self.growth)
            .map_err(|error| Error::Io(std::io::Error::other(error)))?;
        let count = proof.facts().len();
        if count > self.budget.0.maximum {
            return Err(Error::Budget);
        }
        let pool = self.budget.0.resident.as_ref().ok_or(Error::Budget)?;
        let admitted = count
            .checked_mul(2 * std::mem::size_of::<ExternalFact>())
            .and_then(|n| n.checked_add(128))
            .ok_or(Error::Budget)?;
        let _charge = pool.reserve_bytes(admitted, 64 * 1024 * 1024)?;
        let mut facts = Vec::new();
        facts.try_reserve_exact(count).map_err(|_| Error::Budget)?;
        if facts
            .capacity()
            .checked_mul(std::mem::size_of::<ExternalFact>())
            .is_none_or(|n| n > admitted)
        {
            return Err(Error::Budget);
        }
        facts.extend(proof.facts().iter().map(|fact| ExternalFact {
            identity: fact.identity(),
            bytes: fact.bytes(),
            modified: fact.modified_nanos(),
        }));
        self.transfer_checked(&facts, proof.retired(), true)
    }

    // This raw accounting operation is deliberately PRIVATE. SourceRuntime may
    // expose it only through Root's closed held-file outcome types, never a
    // serialized fact/count/hash or generic user-provided callback.
    fn transfer(&self, final_facts: &[ExternalFact], retired: &[Identity]) -> Result<()> {
        self.transfer_checked(final_facts, retired, false)
    }
    fn transfer_checked(
        &self,
        final_facts: &[ExternalFact],
        retired: &[Identity],
        closed_root: bool,
    ) -> Result<()> {
        validate(final_facts, self.budget.0.maximum)?;
        let mut state = self.budget.0.state.lock().map_err(|_| Error::Interrupted)?;
        let active = *state
            .publication_targets
            .get(&self.target)
            .ok_or(Error::Changed)?;
        let entry = state.coexistence.get(&self.intent).ok_or(Error::Changed)?;
        if !active.active
            || active.intent != self.intent
            || active.key != self.key
            || entry.settled
            || entry.key != self.key
            || entry.bytes != self.growth
            || retired.len() > if closed_root { 3 } else { 1 }
            || retired
                .iter()
                .enumerate()
                .any(|(at, id)| retired[..at].contains(id))
            || retired
                .iter()
                .any(|id| final_facts.iter().any(|fact| fact.identity == *id))
        {
            return Err(Error::Changed);
        }
        let mut removed = 0u64;
        for id in retired {
            if state
                .files
                .values()
                .any(|entry| entry.identity == Some(*id))
            {
                return Err(Error::Changed);
            }
            match state.known_external.get(id) {
                Some(fact) if Some(*id) == active.replaced => {
                    removed = removed.checked_add(fact.bytes).ok_or(Error::Corrupt)?
                }
                Some(_) => return Err(Error::Changed),
                None if closed_root => (), // Actual newly-created-and-unlinked IDs were covered by growth.
                None => return Err(Error::Changed),
            }
        }
        let mut added = 0u64;
        let mut count = 0usize;
        for fact in final_facts {
            if state
                .files
                .values()
                .any(|entry| entry.identity == Some(fact.identity))
            {
                return Err(Error::Changed);
            }
            match state.known_external.get(&fact.identity) {
                Some(old) if old != fact => return Err(Error::Changed),
                Some(_) => (),
                None => {
                    added = added.checked_add(fact.bytes).ok_or(Error::Budget)?;
                    count += 1;
                }
            }
        }
        if added > self.growth || count > active.slots {
            return Err(Error::Changed);
        }
        let external = state
            .external
            .checked_sub(self.growth)
            .and_then(|n| n.checked_sub(removed))
            .and_then(|n| n.checked_add(added))
            .ok_or(Error::Corrupt)?;
        // Swap growth into observed files in ONE accounting transition. Reserving
        // their bytes a second time would falsely reject a legitimate peak.
        for fact in final_facts {
            state.known_external.insert_reserved(fact.identity, *fact)?;
        }
        state.known_external.retain(|id, _| !retired.contains(id));
        state.coexistence.insert_reserved(
            self.intent,
            ExternalEntry {
                key: self.key,
                bytes: self.growth,
                settled: true,
            },
        )?;
        state.publication_targets.insert_reserved(
            self.target,
            Target {
                active: false,
                ..active
            },
        )?;
        state.external = external;
        state.active_external = state
            .active_external
            .checked_sub(active.slots)
            .ok_or(Error::Corrupt)?;
        Ok(())
    }
}

impl PreparedSettlement {
    pub(crate) fn uses_budget(&self, budget: &DiskBudget) -> bool {
        self.ticket.uses_budget(budget)
    }
    /// All vector capacity and accounting were prepared before publication.
    /// Proof validation still precedes the counter lock and runs on every try.
    pub(crate) fn settle(
        &mut self,
        proof: &dyn crate::file_safety::ExternalOutcomeProof,
    ) -> Result<()> {
        proof
            .validate(self.ticket.intent, self.ticket.target, self.ticket.growth)
            .map_err(|e| Error::Io(std::io::Error::other(e)))?;
        if proof.facts().len() > self.facts.capacity() {
            return Err(Error::Budget);
        }
        self.facts.clear();
        self.facts
            .extend(proof.facts().iter().map(|f| ExternalFact {
                identity: f.identity(),
                bytes: f.bytes(),
                modified: f.modified_nanos(),
            }));
        self.ticket
            .transfer_checked(&self.facts, proof.retired(), true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fact(id: u64, bytes: u64) -> ExternalFact {
        ExternalFact {
            identity: (1, id),
            bytes,
            modified: 91,
        }
    }
    fn budget(bytes: u64) -> DiskBudget {
        DiskBudget::with_resident(bytes, 32, ResidentBudget::new(128 * 1024).unwrap()).unwrap()
    }
    #[test]
    fn exact_peak_transfer_and_repeated_saves_deduplicate_global_physical_files() {
        let b = budget(64);
        let source = b.reserve(30).unwrap();
        let first = b
            .reserve_publication([1; 32], [8; 32], &[fact(1, 6)], Some((1, 1)), 12)
            .unwrap();
        assert_eq!(b.admitted().unwrap(), 48);
        first
            .transfer(&[fact(2, 6), fact(3, 6)], &[(1, 1)])
            .unwrap();
        assert_eq!(b.admitted().unwrap(), 42);
        let next = b
            .reserve_publication(
                [2; 32],
                [8; 32],
                &[fact(2, 6), fact(3, 6)],
                Some((1, 2)),
                12,
            )
            .unwrap();
        assert_eq!(b.admitted().unwrap(), 54);
        next.transfer(&[fact(3, 6), fact(4, 6), fact(5, 6)], &[(1, 2)])
            .unwrap();
        assert_eq!(b.admitted().unwrap(), 48);
        // A second target observes the same backup files: no duplicate charge.
        let other = b
            .reserve_publication([3; 32], [9; 32], &[fact(3, 6), fact(5, 6)], None, 6)
            .unwrap();
        assert_eq!(b.admitted().unwrap(), 54);
        other
            .transfer(&[fact(3, 6), fact(5, 6), fact(6, 6)], &[])
            .unwrap();
        assert_eq!(b.admitted().unwrap(), 54);
        assert!(source.grow(11).is_err());
    }
    #[test]
    fn pending_drop_competitors_lost_receipt_and_stale_handles_keep_all_debt() {
        let b = budget(100);
        let facts = [fact(1, 12)];
        let ticket = b
            .reserve_publication([1; 32], [2; 32], &facts, Some((1, 1)), 40)
            .unwrap();
        let stale = b
            .restore_publication([1; 32], [2; 32], &facts, Some((1, 1)), 40)
            .unwrap();
        assert_eq!(b.admitted().unwrap(), 52);
        assert!(matches!(
            b.reserve_publication([3; 32], [2; 32], &facts, None, 1),
            Err(Error::Busy)
        ));
        drop(ticket);
        assert_eq!(b.admitted().unwrap(), 52);
        assert!(b
            .restore_publication([1; 32], [2; 32], &facts, Some((1, 1)), 39)
            .is_err());
        let resumed = b
            .restore_publication([1; 32], [2; 32], &facts, Some((1, 1)), 40)
            .unwrap();
        resumed.transfer(&facts, &[]).unwrap();
        assert_eq!(b.admitted().unwrap(), 12);
        assert!(stale.transfer(&[], &[]).is_err());
        assert!(b
            .reserve_publication([1; 32], [2; 32], &facts, None, 1)
            .is_err());
    }
    #[test]
    fn partial_abort_keeps_observed_files_and_refuses_unknown_or_foreign_retirement() {
        let b = budget(100);
        let facts = [fact(1, 12)];
        let ticket = b
            .reserve_publication([1; 32], [2; 32], &facts, Some((1, 1)), 40)
            .unwrap();
        assert!(ticket.transfer(&[fact(1, 12), fact(2, 41)], &[]).is_err());
        assert_eq!(b.admitted().unwrap(), 52);
        let ticket = b
            .restore_publication([1; 32], [2; 32], &facts, Some((1, 1)), 40)
            .unwrap();
        assert!(ticket.transfer(&facts, &[(1, 9)]).is_err());
        assert_eq!(b.admitted().unwrap(), 52);
        let ticket = b
            .restore_publication([1; 32], [2; 32], &facts, Some((1, 1)), 40)
            .unwrap();
        ticket.transfer(&[fact(1, 12), fact(2, 7)], &[]).unwrap();
        assert_eq!(b.admitted().unwrap(), 19);
        let changed = [ExternalFact {
            modified: 92,
            ..fact(1, 12)
        }];
        assert!(b
            .reserve_publication([3; 32], [2; 32], &changed, None, 1)
            .is_err());
        assert_eq!(b.admitted().unwrap(), 19);
    }
    #[test]
    fn metadata_refusal_precedes_new_file_debt_and_ticket_capacity_counts_globally() {
        let pool = ResidentBudget::new(1).unwrap();
        let b = DiskBudget::with_resident(100, 4, pool.clone()).unwrap();
        assert!(b
            .reserve_publication([1; 32], [2; 32], &[fact(1, 12)], None, 1)
            .is_err());
        assert_eq!(b.admitted().unwrap(), 0);
        assert_eq!(pool.used(), 0);
        let b = DiskBudget::with_resident(100, 2, ResidentBudget::new(65536).unwrap()).unwrap();
        let ticket = b
            .reserve_publication([1; 32], [2; 32], &[fact(1, 12)], None, 1)
            .unwrap();
        assert!(b.reserve(0).is_err());
        drop(ticket);
        assert_eq!(b.admitted().unwrap(), 13);
    }

    #[test]
    fn sealed_accounting_boundary_refuses_wrong_intent_and_repeated_transfer_without_refund() {
        use crate::file_safety::{AccountingOutcome, NativeExternalFile};
        let b = budget(100);
        let ticket = b
            .reserve_publication([1; 32], [2; 32], &[fact(1, 12)], Some((1, 1)), 40)
            .unwrap();
        let converted = || vec![NativeExternalFile::for_accounting_tests((1, 2), 7, 91)];
        assert!(ticket
            .settle(&AccountingOutcome::new(
                [9; 32],
                [2; 32],
                40,
                converted(),
                vec![(1, 1)]
            ))
            .is_err());
        assert!(ticket
            .settle(
                &AccountingOutcome::new([1; 32], [2; 32], 40, converted(), vec![(1, 1)]).refuse()
            )
            .is_err());
        assert_eq!(b.admitted().unwrap(), 52);
        let proof = AccountingOutcome::new([1; 32], [2; 32], 40, converted(), vec![(1, 1)]);
        ticket.settle(&proof).unwrap();
        assert_eq!(b.admitted().unwrap(), 7);
        assert!(ticket.settle(&proof).is_err());
        assert_eq!(b.admitted().unwrap(), 7);
    }

    #[test]
    fn root_proven_newly_retired_ids_release_only_growth_and_cannot_retire_known_foreign_debt() {
        use crate::file_safety::{AccountingOutcome, NativeExternalFile};
        let b = budget(100);
        let ticket = b
            .reserve_publication(
                [1; 32],
                [2; 32],
                &[fact(1, 12), fact(2, 5)],
                Some((1, 1)),
                40,
            )
            .unwrap();
        let facts = || {
            vec![
                NativeExternalFile::for_accounting_tests((1, 1), 12, 91),
                NativeExternalFile::for_accounting_tests((1, 2), 5, 91),
            ]
        };
        for retired in [
            vec![(1, 2)],
            vec![(1, 9), (1, 9)],
            vec![(1, 9), (1, 10), (1, 11), (1, 12)],
        ] {
            assert!(ticket
                .settle(&AccountingOutcome::new(
                    [1; 32],
                    [2; 32],
                    40,
                    facts(),
                    retired
                ))
                .is_err());
            assert_eq!(b.admitted().unwrap(), 57);
        }
        ticket
            .settle(&AccountingOutcome::new(
                [1; 32],
                [2; 32],
                40,
                facts(),
                vec![(1, 9), (1, 10)],
            ))
            .unwrap();
        assert_eq!(b.admitted().unwrap(), 17); // Existing target and backup remain fully charged.
    }
    #[test]
    fn cold_settled_reconstruction_adds_current_files_once_and_retains_every_intent_tombstone() {
        let b = budget(64);
        let source = b.reserve(30).unwrap();
        b.restore_external([2; 32], [8; 32], &[fact(2, 6), fact(3, 6)], 12)
            .unwrap();
        b.retain_settled_intent([1; 32], 12).unwrap();
        b.retain_settled_intent([2; 32], 12).unwrap();
        assert_eq!(b.admitted().unwrap(), 42);
        b.restore_external([2; 32], [8; 32], &[fact(2, 6), fact(3, 6)], 12)
            .unwrap();
        assert_eq!(b.admitted().unwrap(), 42);
        for intent in [[1; 32], [2; 32]] {
            assert!(b
                .reserve_publication(intent, [8; 32], &[fact(2, 6), fact(3, 6)], Some((1, 2)), 12)
                .is_err());
        }
        let next = b
            .reserve_publication(
                [3; 32],
                [8; 32],
                &[fact(2, 6), fact(3, 6)],
                Some((1, 2)),
                12,
            )
            .unwrap();
        assert_eq!(b.admitted().unwrap(), 54);
        next.transfer(&[fact(3, 6), fact(4, 6), fact(5, 6)], &[(1, 2)])
            .unwrap();
        assert_eq!(b.admitted().unwrap(), 48);
        assert!(source.grow(17).is_err());
    }
    #[test]
    fn decoded_settled_data_can_never_refund_a_live_pending_operation() {
        let b = budget(100);
        let pending = b
            .reserve_publication([1; 32], [2; 32], &[fact(1, 12)], Some((1, 1)), 40)
            .unwrap();
        assert!(b
            .restore_external([1; 32], [2; 32], &[fact(1, 12)], 40)
            .is_err());
        assert!(b.retain_settled_intent([1; 32], 40).is_err());
        assert_eq!(b.admitted().unwrap(), 52);
        drop(pending);
        assert_eq!(b.admitted().unwrap(), 52);
    }
    #[test]
    fn prepared_settlement_needs_no_fresh_metadata_capacity_after_commit() {
        use crate::file_safety::{AccountingOutcome, NativeExternalFile};
        let b = budget(100);
        let ticket = b
            .reserve_publication([1; 32], [2; 32], &[fact(1, 12)], Some((1, 1)), 40)
            .unwrap();
        let mut prepared = ticket.prepare_settlement(2).unwrap();
        assert!(prepared.uses_budget(&b));
        let pool = b.0.resident.as_ref().unwrap();
        let remainder = pool.limit() - pool.used();
        let full = pool.reserve_bytes(remainder, pool.limit()).unwrap();
        let proof = AccountingOutcome::new(
            [1; 32],
            [2; 32],
            40,
            vec![NativeExternalFile::for_accounting_tests((1, 2), 7, 91)],
            vec![(1, 1)],
        );
        assert!(ticket.settle(&proof).is_err());
        assert_eq!(b.admitted().unwrap(), 52);
        prepared.settle(&proof).unwrap();
        assert_eq!(b.admitted().unwrap(), 7);
        assert!(prepared.settle(&proof).is_err());
        drop(full);
    }
    #[test]
    fn readonly_pending_reused_identity_keeps_both_debts_and_cannot_retire_the_internal_file() {
        use crate::file_safety::{AccountingOutcome, NativeExternalFile};
        let path =
            std::env::temp_dir().join(format!("peerbrush-pending-id-{}", uuid::Uuid::new_v4()));
        let mut file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(&path)
            .unwrap();
        use std::io::Write;
        file.write_all(b"source!").unwrap();
        file.sync_all().unwrap();
        let id = crate::file_safety::identity(&file).unwrap();
        let b = budget(100);
        let source = b.reserve(7).unwrap();
        source.bind(&file).unwrap();
        let old = ExternalFact {
            identity: id,
            bytes: 12,
            modified: 91,
        };
        let pending = b
            .restore_publication([1; 32], [2; 32], &[old], Some(id), 40)
            .unwrap();
        assert_eq!(b.admitted().unwrap(), 59);
        assert!(b
            .reserve_publication([3; 32], [4; 32], &[old], Some(id), 1)
            .is_err());
        let proof = AccountingOutcome::new(
            [1; 32],
            [2; 32],
            40,
            vec![NativeExternalFile::for_accounting_tests(
                (id.0, id.1.wrapping_add(1)),
                1,
                91,
            )],
            vec![id],
        );
        assert!(pending.settle(&proof).is_err());
        assert_eq!(b.admitted().unwrap(), 59);
        assert_eq!(std::fs::read(&path).unwrap(), b"source!");
        assert_eq!(crate::file_safety::identity(&file).unwrap(), id);
        drop(pending);
        drop(source);
        drop(b);
        drop(file);
        let named = crate::file_safety::open_native_identity(&path).unwrap();
        assert_eq!(crate::file_safety::identity(&named).unwrap(), id);
        drop(named);
        std::fs::remove_file(path).unwrap();
    }
}
