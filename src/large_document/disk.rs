//! Shared logical-file admission. Dropping a ticket never frees disk capacity.
use super::{Error, Result};
use std::{
    collections::BTreeMap,
    fs::File,
    path::Path,
    sync::{Arc, Mutex},
};

type Identity = (u64, u64);

#[derive(Clone)]
pub struct DiskBudget(Arc<Inner>);
struct Inner {
    limit: u64,
    maximum: usize,
    state: Mutex<State>,
}
struct State {
    used: u64,
    next: u64,
    files: BTreeMap<u64, Entry>,
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
            let named = crate::file_safety::open_for_identity(path).map_err(|_| Error::Changed)?;
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
        if limit == 0 || maximum_files == 0 || maximum_files > 65536 {
            return Err(Error::Budget);
        }
        Ok(Self(Arc::new(Inner {
            limit,
            maximum: maximum_files,
            state: Mutex::new(State {
                used: 0,
                next: 0,
                files: BTreeMap::new(),
            }),
        })))
    }
    pub fn used(&self) -> Result<u64> {
        Ok(self.0.state.lock().map_err(|_| Error::Interrupted)?.used)
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
            .filter(|v| *v <= self.0.limit)
            .ok_or(Error::Budget)?;
        if state.files.len() >= self.0.maximum {
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
        let named = crate::file_safety::open_for_identity(path).map_err(|_| Error::Changed)?;
        let before = super::store::stamp(file)?;
        if crate::file_safety::identity(&named).map_err(|_| Error::Changed)? != identity
            || super::store::links(file)? != 1
        {
            return Err(Error::Changed);
        }
        let mut state = self.0.state.lock().map_err(|_| Error::Interrupted)?;
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
            if state.files.len() >= self.0.maximum {
                return Err(Error::Budget);
            }
            let used = state
                .used
                .checked_add(before.bytes)
                .filter(|v| *v <= self.0.limit)
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
        let after = crate::file_safety::open_for_identity(path).map_err(|_| Error::Changed)?;
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
        if state
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
            .filter(|v| *v <= self.budget.0.limit)
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

#[cfg(test)]
mod tests {
    use super::*;
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
