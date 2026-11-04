//! Fallible source indexes. Capacity admission precedes every replacement buffer.
use super::{Error, Reservation, ResidentBudget, Result};

pub(super) struct Index<K, V> {
    entries: Vec<(K, V)>,
    // The buffer and its entries drop before the conservative capacity admission.
    charges: [Option<Reservation>; 4],
    pool: Option<ResidentBudget>,
}
impl<K: Ord + Clone, V: Clone> Index<K, V> {
    pub fn new() -> Self {
        Self {
            entries: Vec::new(),
            charges: std::array::from_fn(|_| None),
            pool: None,
        }
    }
    pub fn len(&self) -> usize {
        self.entries.len()
    }
    pub fn get(&self, key: &K) -> Option<&V> {
        self.entries
            .binary_search_by(|(existing, _)| existing.cmp(key))
            .ok()
            .map(|at| &self.entries[at].1)
    }
    pub fn retain(&mut self, mut keep: impl FnMut(&K, &V) -> bool) {
        self.entries.retain(|(key, value)| keep(key, value));
    }
    /// K/V clones here must be cheap metadata copies (digest/BlockRef/Weak), not
    /// independently allocated payloads. Only private source/ledger indexes use it.
    pub fn reserve_one(&mut self, budget: &ResidentBudget, maximum: usize) -> Result<()> {
        self.reserve_to(
            self.entries.len().checked_add(1).ok_or(Error::Budget)?,
            budget,
            maximum,
        )
    }
    pub fn reserve_to(
        &mut self,
        needed: usize,
        budget: &ResidentBudget,
        maximum: usize,
    ) -> Result<()> {
        if self
            .pool
            .as_ref()
            .is_some_and(|pool| !pool.same_pool(budget))
        {
            return Err(Error::Budget);
        }
        if needed > maximum {
            return Err(Error::Budget);
        }
        if needed <= self.entries.capacity() {
            return Ok(());
        }
        let capacity = self
            .entries
            .capacity()
            .checked_mul(2)
            .ok_or(Error::Budget)?
            .max(needed)
            .min(maximum);
        let payload = capacity
            .checked_mul(std::mem::size_of::<(K, V)>())
            .ok_or(Error::Budget)?;
        if payload == 0 {
            return Err(Error::Invalid(
                "Source index entries must own nonzero metadata",
            ));
        }
        // Conservative 2x capacity covers allocation granularity and the index's
        // small reservation bookkeeping. The old owner stays charged throughout
        // fallible allocation and clone; this is not a process-RAM measurement.
        let admitted = payload.checked_mul(2).ok_or(Error::Budget)?;
        let mut remaining = admitted;
        if admitted > 4 * 64 * 1024 * 1024 {
            return Err(Error::Budget);
        }
        let mut charges = std::array::from_fn(|_| None);
        for slot in &mut charges {
            if remaining == 0 {
                break;
            }
            let count = remaining.min(64 * 1024 * 1024);
            *slot = Some(budget.reserve_index(count)?);
            remaining -= count;
        }
        let mut entries = Vec::new();
        entries
            .try_reserve_exact(capacity)
            .map_err(|_| Error::Budget)?;
        if entries
            .capacity()
            .checked_mul(std::mem::size_of::<(K, V)>())
            .is_none_or(|bytes| bytes > admitted)
        {
            return Err(Error::Budget);
        }
        for (key, value) in &self.entries {
            entries.push((key.clone(), value.clone()));
        }
        let old = std::mem::replace(
            self,
            Self {
                entries,
                charges,
                pool: Some(budget.clone()),
            },
        );
        drop(old);
        Ok(())
    }
    pub fn insert_reserved(&mut self, key: K, value: V) -> Result<Option<V>> {
        match self
            .entries
            .binary_search_by(|(existing, _)| existing.cmp(&key))
        {
            Ok(at) => Ok(Some(std::mem::replace(&mut self.entries[at].1, value))),
            Err(at) => {
                if self.entries.len() == self.entries.capacity() {
                    return Err(Error::Budget);
                }
                self.entries.insert(at, (key, value));
                Ok(None)
            }
        }
    }
    #[cfg(test)]
    pub fn admitted_bytes(&self) -> usize {
        self.charges.iter().flatten().map(Reservation::bytes).sum()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn exact_order_and_conservative_capacity_survive_growth_and_owner_drop() {
        let pool = ResidentBudget::new(4096).unwrap();
        let mut index = Index::new();
        for value in (0u64..33).rev() {
            index.reserve_one(&pool, 64).unwrap();
            assert_eq!(index.insert_reserved(value, value * 9).unwrap(), None);
        }
        assert_eq!(pool.used(), index.admitted_bytes());
        for value in 0..33 {
            assert_eq!(index.get(&value), Some(&(value * 9)));
        }
        index.retain(|key, _| key % 2 == 0);
        assert_eq!(pool.used(), index.admitted_bytes());
        assert_eq!(index.get(&3), None);
        assert_eq!(index.get(&4), Some(&36));
        drop(index);
        assert_eq!(pool.used(), 0);
    }
    #[test]
    fn growth_refusal_keeps_existing_entries_and_does_not_leak_partial_admission() {
        let pool = ResidentBudget::new(192).unwrap();
        let mut index = Index::new();
        for value in 0u64..4 {
            index.reserve_one(&pool, 16).unwrap();
            index.insert_reserved(value, value).unwrap();
        }
        assert_eq!(pool.used(), 128);
        assert!(matches!(index.reserve_one(&pool, 16), Err(Error::Budget)));
        assert_eq!(pool.used(), 128);
        assert_eq!(index.len(), 4);
        for value in 0..4 {
            assert_eq!(index.get(&value), Some(&value));
        }
        let foreign = ResidentBudget::new(192).unwrap();
        assert!(index.reserve_one(&foreign, 16).is_err());
        assert_eq!(foreign.used(), 0);
        drop(index);
        assert_eq!(pool.used(), 0);
    }
}
