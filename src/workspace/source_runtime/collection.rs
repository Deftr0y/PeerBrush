//! Exclusive retirement of actual retained sources. Counts and weak absence
//! remain observations; only a claimed backing plus complete root proof can retire.
use super::*;
use std::cell::{Cell, RefCell};

struct Observation {
    held: HeldFile,
    at: usize,
}
pub(crate) struct SourceInspection {
    pub(crate) path: PathBuf,
    pub(crate) identity: [u8; 16],
    pub(crate) filesystem: ((u64, u64), (u64, u64)),
    pub(crate) resident: ResidentBudget,
    pub(crate) disk: DiskBudget,
    pub(crate) limits: StoreLimits,
}

pub(crate) struct Collection {
    pub(crate) sources: usize,
    pub(crate) bytes: u64,
}
#[cfg(test)]
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum CollectionFault {
    File,
    Directory,
    Settlement,
    PanicFile,
    YieldFile,
}
/// Unforgeable outside this module; the exclusive namespace guard outlives it.
pub(crate) struct SourceRetirement<'a> {
    session: &'a SourceSession,
    _exclusive: &'a crate::large_document::SourceExclusive,
    identity: [u8; 16],
    filesystem: ((u64, u64), (u64, u64)),
    attempted: Cell<bool>,
    settled: Cell<bool>,
    cold: bool,
    observation: RefCell<Option<Observation>>,
    #[cfg(test)]
    fault: Option<CollectionFault>,
}
impl SourceRetirement<'_> {
    #[cfg(test)]
    pub(crate) fn fail(&self, phase: CollectionFault) -> Result<(), String> {
        if phase == CollectionFault::File && self.fault == Some(CollectionFault::YieldFile) {
            self.session.runtime.maintenance.interrupt_for_test();
        }
        if phase == CollectionFault::File && self.fault == Some(CollectionFault::PanicFile) {
            panic!("Interrupted after source unlink");
        }
        if self.fault == Some(phase) {
            return Err("Injected source retirement failure".into());
        }
        Ok(())
    }
    pub(crate) fn check(
        &self,
        identity: [u8; 16],
        filesystem: ((u64, u64), (u64, u64)),
        resident: &ResidentBudget,
        disk: &DiskBudget,
    ) -> Result<(), String> {
        if identity != self.identity
            || filesystem != self.filesystem
            || !resident.same_pool(&self.session.resident())
            || !disk.same_pool(&self.session.disk())
        {
            return Err("Source retirement differs from its actual admission".into());
        }
        self.check_namespace()?;
        if let Some(observed) = self.observation.borrow().as_ref() {
            observed.held.check()?;
        } else if self.cold && !self.attempted.get() {
            return Err("Unselected source has no retained observation".into());
        }
        self.session.checkpoint_census(&|| self.check_namespace())?;
        let state = self
            .session
            .runtime
            .inventory
            .try_lock()
            .map_err(|_| "Source inventory is busy")?;
        let inventory = state.as_ref().ok_or("Source inventory unavailable")?;
        let location = inventory
            .locations
            .get(&identity)
            .ok_or("Source retirement owner disappeared")?;
        if let Some(observation) = self.observation.borrow().as_ref() {
            if observation.held.identity != filesystem.1
                || observation.held.path != location.path.join("blocks.pbt")
            {
                return Err("Source observation differs from its actual inventory".into());
            }
        }
        if (location.directory_identity, location.file_identity) != filesystem
            || (self.cold && location.weak.is_some())
            || location
                .weak
                .as_ref()
                .and_then(WeakStore::upgrade)
                .is_some()
            || inventory
                .files
                .iter()
                .any(|held| held.identity == filesystem.1)
        {
            return Err("Source retirement gained a live or duplicate owner".into());
        }
        Ok(())
    }
    pub(crate) fn inspection(&self) -> Result<SourceInspection, String> {
        if !self.cold {
            return Err("Source is already selected".into());
        }
        let observation = self.observation.borrow();
        let held = &observation
            .as_ref()
            .ok_or("Unselected source observation unavailable")?
            .held;
        held.check()?;
        self.check(
            self.identity,
            self.filesystem,
            &self.session.resident(),
            &self.session.disk(),
        )?;
        Ok(SourceInspection {
            path: held
                .path
                .parent()
                .ok_or("Invalid source parent")?
                .to_owned(),
            identity: self.identity,
            filesystem: self.filesystem,
            resident: self.session.resident(),
            disk: self.session.disk(),
            limits: self.session.runtime.limits.store,
        })
    }
    pub(crate) fn close_observation(&self) -> Result<(), String> {
        if !self.attempted.get() {
            return Err("Source unlink has not been attempted".into());
        }
        // The exclusive guard remains held; no inventory/Engine lock spans close.
        drop(self.observation.borrow_mut().take());
        Ok(())
    }
    /// After this point, uncertainty blocks fresh admission and quota refunds.
    pub(crate) fn attempt(&self) -> Result<(), String> {
        self.session.runtime.maintenance.check()?;
        self.attempted.set(true);
        Ok(())
    }
    pub(crate) fn check_namespace(&self) -> Result<(), String> {
        if !self.attempted.get() {
            self.session.runtime.maintenance.check()?;
        }
        self.session.cleanup_check()
    }
    pub(crate) fn parent_identity(&self) -> Result<(u64, u64), String> {
        let state = self
            .session
            .runtime
            .inventory
            .try_lock()
            .map_err(|_| "Source inventory is busy")?;
        Ok(state
            .as_ref()
            .ok_or("Source inventory unavailable")?
            .directory
            .identity())
    }
}
impl Drop for SourceRetirement<'_> {
    fn drop(&mut self) {
        if self.attempted.get() && !self.settled.get() {
            self.session.runtime.tainted.store(true, Ordering::Release);
        }
        if let Some(observation) = self.observation.get_mut().take() {
            let mut state = self
                .session
                .runtime
                .inventory
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            let inventory = state.as_mut().expect("Admitted source inventory");
            inventory.files.insert(observation.at, observation.held);
        }
    }
}
impl SourceSession {
    /// All I/O occurs outside Engine/workspace locks. Other source readers and
    /// retained child permits prevent the exclusive upgrade, rather than racing GC.
    pub(crate) fn collect_sources(
        self,
        check: &dyn Fn() -> Result<(), String>,
    ) -> Result<Collection, String> {
        self.collect_sources_inner(
            check,
            #[cfg(test)]
            None,
        )
    }
    #[cfg(test)]
    pub(crate) fn collect_sources_with_fault(
        self,
        check: &dyn Fn() -> Result<(), String>,
        fault: CollectionFault,
    ) -> Result<Collection, String> {
        self.collect_sources_inner(check, Some(fault))
    }
    fn collect_sources_inner(
        mut self,
        check: &dyn Fn() -> Result<(), String>,
        #[cfg(test)] fault: Option<CollectionFault>,
    ) -> Result<Collection, String> {
        if !cfg!(windows) {
            return Err(
                "Handle-bound source collection is unavailable on this platform; sources retained"
                    .into(),
            );
        }
        check()?;
        let runtime = self.runtime.clone();
        // Drop order releases exclusive access before waking foreground readers.
        let _maintenance = runtime.maintenance.start().map_err(|e| e.to_string())?;
        let exclusive = self
            .access
            .take()
            .ok_or("Source cleanup admission unavailable")?
            .exclusive()
            .map_err(|e| e.to_string())?;
        let check = &|| {
            runtime.maintenance.check()?;
            check()
        };
        self.cleanup_check()?;
        let bytes = self
            .runtime
            .limits
            .max_files
            .checked_mul(32)
            .and_then(|n| n.checked_add(128))
            .ok_or("Source collection index overflow")?;
        let _index = self
            .resident()
            .reserve_bytes(bytes, bytes)
            .map_err(|e| e.to_string())?;
        let mut ids = Vec::new();
        {
            let state = self
                .runtime
                .inventory
                .try_lock()
                .map_err(|_| "Source inventory is busy")?;
            let inventory = state.as_ref().ok_or("Source inventory unavailable")?;
            ids.try_reserve_exact(inventory.locations.len())
                .map_err(|_| "Source collection index allocation refused")?;
            if ids.capacity() > self.runtime.limits.max_files {
                return Err("Source collection index exceeds admission".into());
            }
            ids.extend(inventory.locations.keys().copied());
        }
        let mut collected = Collection {
            sources: 0,
            bytes: 0,
        };
        for identity in ids {
            check()?;
            self.cleanup_check()?;
            let (source, filesystem, observation) = {
                let mut state = self
                    .runtime
                    .inventory
                    .try_lock()
                    .map_err(|_| "Source inventory is busy")?;
                let inventory = state.as_mut().ok_or("Source inventory unavailable")?;
                let location = inventory
                    .locations
                    .get(&identity)
                    .ok_or("Source collection inventory changed")?;
                let filesystem = (location.directory_identity, location.file_identity);
                if let Some(owner) = location.weak.as_ref() {
                    let Some(source) = owner.claim_parked().map_err(|e| e.to_string())? else {
                        continue;
                    };
                    (Some(source), filesystem, None)
                } else {
                    let at = inventory
                        .files
                        .iter()
                        .position(|held| {
                            held.identity == filesystem.1
                                && held.path == location.path.join("blocks.pbt")
                        })
                        .ok_or("Unselected source has no exact held observation")?;
                    let held = inventory.files.remove(at);
                    (None, filesystem, Some(Observation { held, at }))
                }
            };
            let permit = SourceRetirement {
                session: &self,
                _exclusive: &exclusive,
                identity,
                filesystem,
                attempted: Cell::new(false),
                settled: Cell::new(false),
                cold: observation.is_some(),
                observation: RefCell::new(observation),
                #[cfg(test)]
                fault,
            };
            let source = match source {
                Some(source) => source,
                None => crate::large_document::ParkedSource::inspect_unselected(&permit, check)?,
            };
            let (charge, receipt) = source.retire(&permit, check)?;
            let retired_bytes = receipt.bytes();
            #[cfg(test)]
            permit.fail(CollectionFault::Settlement)?;
            // Receipt settlement is bound to the exact logical file and pool.
            charge.settle_source(receipt).map_err(|e| e.to_string())?;
            {
                let mut state = self
                    .runtime
                    .inventory
                    .try_lock()
                    .map_err(|_| "Source inventory is busy")?;
                let inventory = state.as_mut().ok_or("Source inventory unavailable")?;
                let location = inventory
                    .locations
                    .get(&identity)
                    .ok_or("Retired source inventory changed")?;
                if (location.directory_identity, location.file_identity) != permit.filesystem
                    || location
                        .weak
                        .as_ref()
                        .and_then(WeakStore::upgrade)
                        .is_some()
                {
                    return Err("Retired source gained an unexpected owner".into());
                }
                inventory.locations.remove(&identity);
                inventory.lineages.retain(|_, source| *source != identity);
                inventory.directories = inventory
                    .directories
                    .checked_sub(1)
                    .ok_or("Retired source directory count differs")?;
            }
            permit.settled.set(true);
            collected.sources += 1;
            collected.bytes = collected
                .bytes
                .checked_add(retired_bytes)
                .ok_or("Collected source byte count overflow")?;
        }
        self.cleanup_check()?;
        if collected.sources != 0 {
            let yielded = Cell::new(false);
            if let Err(error) = self.checkpoint_census(&|| {
                if let Err(error) = runtime.maintenance.check() {
                    yielded.set(true);
                    return Err(error);
                }
                self.cleanup_check()
            }) {
                if !yielded.get() {
                    self.runtime.tainted.store(true, Ordering::Release);
                }
                return Err(error);
            }
        }
        check()?;
        Ok(collected)
    }
}
#[cfg(test)]
#[path = "collection/tests.rs"]
mod tests;
