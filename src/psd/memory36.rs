//! Explicit combined metadata/native scratch admission, separate from process RAM.
use super::canonical36::{Canonical, Limits, Stats, MAX_BYTES};
use crate::large_document::{Reservation, ResidentBudget};
use std::{ops::Deref, sync::Arc};

/// Coexisting allocation envelopes in addition to two measured DOM/sort/parser
/// envelopes (manifest and transient canonical core). The raw input owner must
/// already be charged before decompression.
#[derive(Clone, Copy, Default)]
pub(crate) struct WorkingSet {
    pub raw_bytes: usize,
    pub canonical_core_bytes: usize,
    pub canonical_manifest_bytes: usize,
    pub typed_metadata_bytes: usize,
    pub tile_scratch_bytes: usize,
    pub index_scratch_bytes: usize,
}
impl WorkingSet {
    pub(crate) fn total(self, stats: &Stats) -> Result<usize, String> {
        if self.raw_bytes < stats.input_bytes()
            || self.raw_bytes > MAX_BYTES
            || self.canonical_core_bytes > MAX_BYTES
            || self.canonical_manifest_bytes > MAX_BYTES
            || self.canonical_manifest_bytes
                < stats
                    .input_bytes()
                    .checked_next_power_of_two()
                    .unwrap_or(MAX_BYTES)
                    .min(MAX_BYTES)
            || self.tile_scratch_bytes > super::bulk36::MAX_TILE_BYTES
        {
            return Err("Source36 combined working-set declarations differ".into());
        }
        [
            self.raw_bytes,
            self.canonical_core_bytes,
            self.canonical_manifest_bytes,
            self.typed_metadata_bytes,
            self.tile_scratch_bytes,
            self.index_scratch_bytes,
        ]
        .into_iter()
        .try_fold(
            stats
                .tree_upper_bytes()?
                .checked_mul(2)
                .ok_or("Source36 simultaneous tree footprint overflow")?,
            |sum, bytes| {
                sum.checked_add(bytes)
                    .ok_or_else(|| "Source36 combined working-set overflow".into())
            },
        )
    }
}

/// Charge-only owner, intentionally separate from the payload to avoid cycles.
/// Keep this owner with every moved large field; clones of the owner do not
/// authorize new deep allocations. Header/candidate copies require new admission.
pub(crate) struct MetadataCharge {
    budget: ResidentBudget,
    bytes: usize,
    _reservations: Vec<Reservation>,
    // Flat unique reservation owners. No payload graph is retained here.
    _retained: Vec<Arc<MetadataCharge>>,
}
impl MetadataCharge {
    pub(crate) fn reserve(
        budget: &ResidentBudget,
        bytes: usize,
        check: &dyn Fn() -> Result<(), String>,
    ) -> Result<Arc<Self>, String> {
        check()?;
        let count = bytes.div_ceil(MAX_BYTES);
        let mut reservations = Vec::new();
        reservations
            .try_reserve_exact(count)
            .map_err(|_| "Source36 reservation owner allocation refused")?;
        let mut remaining = bytes;
        while remaining != 0 {
            check()?;
            let amount = remaining.min(MAX_BYTES);
            reservations.push(
                budget
                    .reserve_bytes(amount, MAX_BYTES)
                    .map_err(|e| e.to_string())?,
            );
            remaining -= amount;
        }
        check()?;
        Ok(Arc::new(Self {
            budget: budget.clone(),
            bytes,
            _reservations: reservations,
            _retained: Vec::new(),
        }))
    }
    /// Retain existing charges across captured-field moves without reserving or
    /// cloning their payloads. The input/base admission must already cover this
    /// bounded owner-vector/sort scratch (at most 1,048,576 Arc entries). Flatten
    /// and deduplicate leaves so shared/nested owners cannot inflate byte credit.
    pub(crate) fn combine(
        owners: &[Arc<Self>],
        check: &dyn Fn() -> Result<(), String>,
    ) -> Result<Arc<Self>, String> {
        check()?;
        let budget = owners
            .first()
            .ok_or("Source36 metadata ownership list is empty")?
            .budget
            .clone();
        let count = owners.iter().try_fold(0usize, |n, owner| {
            if !owner.uses_budget(&budget) {
                return Err("Source36 metadata owners use different resident pools");
            }
            n.checked_add(owner._retained.len().max(1))
                .filter(|n| *n <= 1_048_576)
                .ok_or("Source36 metadata ownership index exceeds admission")
        })?;
        let mut retained = Vec::new();
        retained
            .try_reserve_exact(count)
            .map_err(|_| "Source36 retained owner allocation refused")?;
        for owner in owners {
            check()?;
            if owner._retained.is_empty() {
                retained.push(owner.clone());
            } else {
                for leaf in &owner._retained {
                    check()?;
                    if !leaf._retained.is_empty() || !leaf.uses_budget(&budget) {
                        return Err("Source36 metadata retained-owner inventory differs".into());
                    }
                    retained.push(leaf.clone());
                }
            }
        }
        retained.sort_unstable_by_key(|owner| Arc::as_ptr(owner) as usize);
        retained.dedup_by(|a, b| Arc::ptr_eq(a, b));
        let bytes = retained.iter().try_fold(0usize, |n, owner| {
            n.checked_add(owner.bytes)
                .ok_or("Source36 retained metadata byte sum overflow")
        })?;
        check()?;
        Ok(Arc::new(Self {
            budget,
            bytes,
            _reservations: Vec::new(),
            _retained: retained,
        }))
    }
    pub(crate) fn bytes(&self) -> usize {
        self.bytes
    }
    pub(crate) fn uses_budget(&self, budget: &ResidentBudget) -> bool {
        self.budget.same_pool(budget)
    }
}

/// Clones share the same payload allocation and accounting owner. Fields are
/// declared so payload storage drops before its last charge. No bare into_inner.
pub(crate) struct Admitted<T> {
    value: Arc<T>,
    owner: Arc<MetadataCharge>,
}
impl<T> Clone for Admitted<T> {
    fn clone(&self) -> Self {
        Self {
            value: self.value.clone(),
            owner: self.owner.clone(),
        }
    }
}
impl<T> Deref for Admitted<T> {
    type Target = T;
    fn deref(&self) -> &T {
        &self.value
    }
}
impl<T> Admitted<T> {
    pub(crate) fn from_parts(value: Arc<T>, owner: Arc<MetadataCharge>) -> Self {
        Self { value, owner }
    }
    pub(crate) fn owner(&self) -> &Arc<MetadataCharge> {
        &self.owner
    }
    /// Transfer an already admitted side allocation into the same payload
    /// lifetime. No payload clone, new reservation or uncharged extraction.
    pub(crate) fn retain_owner(
        mut self,
        owner: &Arc<MetadataCharge>,
        check: &dyn Fn() -> Result<(), String>,
    ) -> Result<Self, String> {
        self.owner = MetadataCharge::combine(&[self.owner.clone(), owner.clone()], check)?;
        Ok(self)
    }
    /// The closure's new allocations must fit the already-reserved typed/candidate
    /// envelope. The owner travels through errors and the successful handoff.
    pub(crate) fn map_unique<U>(
        self,
        map: impl FnOnce(T, &Arc<MetadataCharge>) -> Result<U, String>,
    ) -> Result<Admitted<U>, String> {
        let Self { value, owner } = self;
        let value = Arc::try_unwrap(value)
            .map_err(|_| "Source36 metadata owner is shared; cannot move its fields")?;
        let value = map(value, &owner)?;
        Ok(Admitted {
            value: Arc::new(value),
            owner,
        })
    }
}

pub(crate) struct Admission {
    stats: Stats,
    limits: Limits,
    owner: Arc<MetadataCharge>,
    raw_owner: Arc<MetadataCharge>,
}
impl Admission {
    /// `raw_owner` keeps the already-allocated input charged. We deliberately
    /// reserve the complete coexistence envelope too: capacity refusal is safe,
    /// and integration may transfer raw accounting only after that owner drops.
    pub(crate) fn reserve(
        stats: Stats,
        limits: Limits,
        working: WorkingSet,
        budget: &ResidentBudget,
        raw_owner: &Arc<MetadataCharge>,
        check: &dyn Fn() -> Result<(), String>,
    ) -> Result<Self, String> {
        limits.validate()?;
        if !raw_owner.uses_budget(budget) || raw_owner.bytes() < working.raw_bytes {
            return Err("Source36 raw metadata owner belongs to another budget or envelope".into());
        }
        let bytes = working.total(&stats)?;
        let owner = MetadataCharge::reserve(budget, bytes, check)?;
        Ok(Self {
            stats,
            limits,
            owner,
            raw_owner: raw_owner.clone(),
        })
    }
    pub(crate) fn parse_owned(
        self,
        bytes: Vec<u8>,
        check: &dyn Fn() -> Result<(), String>,
    ) -> Result<Admitted<Canonical>, String> {
        self.stats.matches(&bytes, check)?;
        if bytes.capacity() > self.raw_owner.bytes() {
            return Err("Source36 raw metadata allocation exceeds its retained owner".into());
        }
        let value = Canonical::parse(&bytes, self.limits, check)?;
        // Original input is gone before the resulting owner can be handed off.
        drop(bytes);
        Ok(Admitted {
            value: Arc::new(value),
            owner: self.owner,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn limits() -> Limits {
        Limits {
            bytes: 4096,
            nodes: 1024,
            depth: 32,
        }
    }
    fn working(bytes: usize) -> WorkingSet {
        let capacity = bytes.next_power_of_two();
        WorkingSet {
            raw_bytes: bytes,
            canonical_core_bytes: capacity,
            canonical_manifest_bytes: capacity,
            typed_metadata_bytes: 8192,
            tile_scratch_bytes: super::super::bulk36::MAX_TILE_BYTES,
            index_scratch_bytes: 4096,
        }
    }
    #[test]
    fn combined_source_metadata_and_native_scratch_admission_precedes_dom_and_shares_one_pool() {
        let bytes = br#"{"a":[1,2,3],"b":"metadata"}"#.to_vec();
        let check = || Ok(());
        let stats = Stats::scan(&bytes, limits(), &check).unwrap();
        let working = working(bytes.len());
        let total = working.total(&stats).unwrap();
        let pool = ResidentBudget::new(total + bytes.len()).unwrap();
        let raw = MetadataCharge::reserve(&pool, bytes.len(), &check).unwrap();
        let admission = Admission::reserve(stats, limits(), working, &pool, &raw, &check).unwrap();
        assert_eq!(pool.used(), total + bytes.len());
        let canonical = admission.parse_owned(bytes, &check).unwrap();
        drop(raw);
        assert_eq!(pool.used(), total);
        let cloned = canonical.clone();
        drop(canonical);
        assert_eq!(pool.used(), total);
        assert!(cloned.owner().uses_budget(&pool));
        drop(cloned);
        assert_eq!(pool.used(), 0);
    }
    #[test]
    fn refused_or_cancelled_multi_segment_reservations_release_every_partial_charge() {
        let check = || Ok(());
        let pool = ResidentBudget::new(MAX_BYTES + 1).unwrap();
        assert!(MetadataCharge::reserve(&pool, MAX_BYTES + 2, &check).is_err());
        assert_eq!(pool.used(), 0);
        let calls = std::cell::Cell::new(0);
        let canceled = || {
            calls.set(calls.get() + 1);
            if calls.get() > 2 {
                Err("Cancelled metadata reservation".into())
            } else {
                Ok(())
            }
        };
        assert!(MetadataCharge::reserve(&pool, MAX_BYTES + 1, &canceled).is_err());
        assert_eq!(pool.used(), 0);
    }
    #[test]
    fn combined_capture_owners_retain_shared_unique_leaf_charges_without_payload_or_budget_duplication(
    ) {
        let check = || Ok(());
        let pool = ResidentBudget::new(2 * 1024 * 1024).unwrap();
        let base = MetadataCharge::reserve(&pool, 1024 * 1024, &check).unwrap();
        let coverage = MetadataCharge::reserve(&pool, 65536, &check).unwrap();
        let expected = pool.used();
        let nested = MetadataCharge::combine(&[base.clone(), coverage.clone()], &check).unwrap();
        let combined = MetadataCharge::combine(
            &[
                nested.clone(),
                coverage.clone(),
                base.clone(),
                nested.clone(),
            ],
            &check,
        )
        .unwrap();
        assert_eq!(combined.bytes(), expected);
        assert_eq!(pool.used(), expected);
        drop(nested);
        drop(base);
        drop(coverage);
        let metadata = Admitted::from_parts(Arc::new(vec!["captured contours"]), combined);
        assert_eq!(pool.used(), expected);
        let retained = metadata.clone();
        drop(metadata);
        assert_eq!(pool.used(), expected);
        drop(retained);
        assert_eq!(pool.used(), 0);
    }
    #[test]
    fn retained_metadata_rejects_foreign_pool_and_cancellation_without_losing_original_charges() {
        let check = || Ok(());
        let pool = ResidentBudget::new(1024 * 1024).unwrap();
        let other = ResidentBudget::new(1024 * 1024).unwrap();
        let a = MetadataCharge::reserve(&pool, 65536, &check).unwrap();
        let b = MetadataCharge::reserve(&other, 65536, &check).unwrap();
        assert!(MetadataCharge::combine(&[a.clone(), b.clone()], &check).is_err());
        assert_eq!(pool.used(), 65536);
        assert_eq!(other.used(), 65536);
        assert!(
            MetadataCharge::combine(&[a.clone()], &|| Err("Cancelled capture".into())).is_err()
        );
        assert_eq!(pool.used(), 65536);
        drop(a);
        drop(b);
        assert_eq!(pool.used(), 0);
        assert_eq!(other.used(), 0);
    }
    #[test]
    fn admitted_side_owner_transfer_preserves_exact_payload_and_both_existing_charges() {
        let check = || Ok(());
        let pool = ResidentBudget::new(1024 * 1024).unwrap();
        let a = MetadataCharge::reserve(&pool, 65536, &check).unwrap();
        let b = MetadataCharge::reserve(&pool, 32768, &check).unwrap();
        let payload = Admitted::from_parts(Arc::new(vec![1u8, 2, 3]), a.clone());
        let pointer = payload.as_ptr();
        let payload = payload.retain_owner(&b, &check).unwrap();
        assert_eq!(payload.as_ptr(), pointer);
        assert_eq!(payload.owner().bytes(), 98304);
        drop(a);
        drop(b);
        assert_eq!(pool.used(), 98304);
        drop(payload);
        assert_eq!(pool.used(), 0);
    }
    #[test]
    fn typed_handoff_keeps_owners_and_refuses_foreign_pools_and_unadmitted_byte_sources() {
        let bytes = br#"{"a":1}"#.to_vec();
        let check = || Ok(());
        let stats = Stats::scan(&bytes, limits(), &check).unwrap();
        let pool = ResidentBudget::new(1024 * 1024).unwrap();
        let raw = MetadataCharge::reserve(&pool, bytes.len(), &check).unwrap();
        let foreign = ResidentBudget::new(1024 * 1024).unwrap();
        assert!(Admission::reserve(
            stats,
            limits(),
            working(bytes.len()),
            &foreign,
            &raw,
            &check
        )
        .is_err());
        let stats = Stats::scan(&bytes, limits(), &check).unwrap();
        let admission =
            Admission::reserve(stats, limits(), working(bytes.len()), &pool, &raw, &check).unwrap();
        let admitted = admission.parse_owned(bytes, &check).unwrap();
        drop(raw);
        let before = pool.used();
        let typed = admitted
            .map_unique(|canonical, owner| {
                let (_, value) = canonical.into_parts();
                Ok(Admitted::from_parts(Arc::new(value), owner.clone()))
            })
            .unwrap();
        let field = (*typed).clone();
        drop(typed);
        assert_eq!(pool.used(), before);
        assert_eq!(field["a"], 1);
        drop(field);
        assert_eq!(pool.used(), 0);
    }

    #[test]
    fn sixty_four_mib_policy_retains_a_larger_style_metadata_byte_envelope_under_shared_admission()
    {
        // Byte-envelope/accounting coverage only: this synthetic string does
        // not qualify a PNG effect, editor rendering or total process RAM.
        let check = || Ok(());
        let pool = ResidentBudget::new(768 * 1024 * 1024).unwrap();
        let raw = MetadataCharge::reserve(&pool, MAX_BYTES, &check).unwrap();
        let mut bytes = Vec::new();
        bytes.try_reserve_exact(MAX_BYTES).unwrap();
        bytes.extend_from_slice(b"{\"assets\":\"");
        bytes.resize(bytes.len() + 32 * 1024 * 1024, b'a');
        bytes.extend_from_slice(b"\"}");
        let limits = Limits {
            bytes: MAX_BYTES,
            nodes: 8_388_608,
            depth: 128,
        };
        let stats = Stats::scan(&bytes, limits, &check).unwrap();
        let working = WorkingSet {
            raw_bytes: MAX_BYTES,
            canonical_core_bytes: MAX_BYTES,
            canonical_manifest_bytes: MAX_BYTES,
            typed_metadata_bytes: 32 * 1024 * 1024,
            tile_scratch_bytes: super::super::bulk36::MAX_TILE_BYTES,
            index_scratch_bytes: super::super::bulk36::BulkVerifier::index_bytes(3969, 1).unwrap(),
        };
        let total = working.total(&stats).unwrap();
        let admitted = Admission::reserve(stats, limits, working, &pool, &raw, &check)
            .unwrap()
            .parse_owned(bytes, &check)
            .unwrap();
        drop(raw);
        assert_eq!(pool.used(), total);
        assert_eq!(
            admitted.value()["assets"].as_str().unwrap().len(),
            32 * 1024 * 1024
        );
        drop(admitted);
        assert_eq!(pool.used(), 0);
    }

    #[test]
    fn large_numeric_metadata_never_bypasses_combined_tree_admission() {
        // Generic numeric metadata only. New36 ICC is typed canonical base64.
        let check = || Ok(());
        let pool = ResidentBudget::new(128 * 1024 * 1024).unwrap();
        let raw = MetadataCharge::reserve(&pool, MAX_BYTES, &check).unwrap();
        let count = 4 * 1024 * 1024;
        let mut bytes = Vec::new();
        bytes.try_reserve_exact(MAX_BYTES).unwrap();
        bytes.extend_from_slice(b"{\"numeric_values\":[");
        for i in 0..count {
            if i != 0 {
                bytes.push(b',');
            }
            bytes.extend_from_slice(b"255");
        }
        bytes.extend_from_slice(b"]}");
        let limits = Limits {
            bytes: MAX_BYTES,
            nodes: 8_388_608,
            depth: 128,
        };
        let stats = Stats::scan(&bytes, limits, &check).unwrap();
        assert_eq!(stats.nodes(), count + 2);
        let capacity = bytes.len().next_power_of_two();
        let working = WorkingSet {
            raw_bytes: MAX_BYTES,
            canonical_core_bytes: capacity,
            canonical_manifest_bytes: capacity,
            typed_metadata_bytes: count,
            tile_scratch_bytes: super::super::bulk36::MAX_TILE_BYTES,
            index_scratch_bytes: 4096,
        };
        assert!(Admission::reserve(stats, limits, working, &pool, &raw, &check).is_err());
        assert_eq!(
            pool.used(),
            MAX_BYTES,
            "No DOM was allocated and partial reservations rolled back"
        );
        drop(bytes);
        drop(raw);
        assert_eq!(pool.used(), 0);
    }
}
