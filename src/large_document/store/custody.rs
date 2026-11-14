//! Actual backing custody survives the last logical source owner. Parking is
//! memory-only; neither weak liveness nor a parked owner grants deletion.
use super::*;
#[path = "custody/retirement.rs"]
mod retirement;
#[path = "custody/unselected.rs"]
mod unselected;
pub(crate) use retirement::RetiredSource;

struct State {
    live: Weak<Inner>,
    parked: Option<Backing>,
}
struct Custody {
    state: Mutex<State>,
    _charge: super::super::Reservation,
}
pub(super) struct Inner {
    backing: Option<Backing>,
    custody: Mutex<Option<Arc<Custody>>>,
}
pub(crate) struct WeakStore(Arc<Custody>);
/// Actual custody, exclusively detached while the source namespace is fenced.
/// Drop restores the same backing without I/O; it is not a deletion permit.
pub(crate) struct ParkedSource {
    store: Option<Store>,
    custody: Option<Arc<Custody>>,
    _inspection: Option<super::super::Reservation>,
}

impl Deref for Inner {
    type Target = Backing;
    fn deref(&self) -> &Backing {
        self.backing.as_ref().expect("Live source backing")
    }
}
impl Inner {
    pub(super) fn new(backing: Backing) -> Arc<Self> {
        Arc::new(Self {
            backing: Some(backing),
            custody: Mutex::new(None),
        })
    }
    pub(super) fn downgrade(owner: &Arc<Self>) -> Result<WeakStore> {
        let mut retained = owner.custody.lock().map_err(|_| Error::Interrupted)?;
        if let Some(custody) = retained.as_ref() {
            return Ok(WeakStore(custody.clone()));
        }
        let bytes = std::mem::size_of::<Custody>()
            .checked_add(2 * std::mem::size_of::<Inner>() + 256)
            .ok_or(Error::Budget)?;
        let charge = owner.budget.reserve_bytes(bytes, bytes)?;
        let custody = Arc::new(Custody {
            state: Mutex::new(State {
                live: Arc::downgrade(owner),
                parked: None,
            }),
            _charge: charge,
        });
        *retained = Some(custody.clone());
        Ok(WeakStore(custody))
    }
    pub(super) fn into_backing(mut self) -> Backing {
        self.backing.take().expect("Unique source backing")
    }
}
impl Drop for Inner {
    fn drop(&mut self) {
        if let Some(backing) = self.backing.take() {
            let custody = self.custody.get_mut().unwrap_or_else(|e| e.into_inner());
            let Some(custody) = custody.as_ref() else {
                // Unregistered component stores retain their existing close policy.
                return;
            };
            // No callbacks, filesystem work or payload/index copies here. A
            // poisoned custodian retains the owner and refuses later resumption.
            let mut state = custody.state.lock().unwrap_or_else(|e| e.into_inner());
            debug_assert!(state.parked.is_none());
            state.parked = Some(backing);
        }
    }
}
impl WeakStore {
    pub(crate) fn claim_parked(&self) -> Result<Option<ParkedSource>> {
        let mut state = self.0.state.try_lock().map_err(|_| Error::Busy)?;
        // A parked Backing exists only after the last logical owner has moved
        // it here. An expired weak count by itself is never sufficient.
        let Some(backing) = state.parked.take() else {
            return Ok(None);
        };
        Ok(Some(ParkedSource {
            store: Some(Store(Inner::new(backing))),
            custody: Some(self.0.clone()),
            _inspection: None,
        }))
    }
    /// Logical liveness only. A parked backing is deliberately not a live root.
    pub(crate) fn upgrade(&self) -> Option<Store> {
        self.0.state.lock().ok()?.live.upgrade().map(Store)
    }
    /// Called only by an actual admitted source session outside Engine locks.
    /// This resumes the same held handles/index, never a guessed newest commit.
    pub(crate) fn resume(&self) -> Result<Store> {
        let mut state = self.0.state.try_lock().map_err(|_| Error::Busy)?;
        if let Some(owner) = state.live.upgrade() {
            return Ok(Store(owner));
        }
        let backing = state.parked.take().ok_or(Error::Busy)?;
        let owner = Arc::new(Inner {
            backing: Some(backing),
            custody: Mutex::new(Some(self.0.clone())),
        });
        state.live = Arc::downgrade(&owner);
        Ok(Store(owner))
    }
}
impl ParkedSource {
    pub(crate) fn identity(&self) -> [u8; 16] {
        self.store.as_ref().unwrap().identity()
    }
    pub(crate) fn filesystem_identity(&self) -> ((u64, u64), (u64, u64)) {
        self.store.as_ref().unwrap().filesystem_identity()
    }
    pub(crate) fn verify(&self, check: Check<'_>) -> Result<()> {
        let store = self.store.as_ref().unwrap();
        // The record verifier streams through a fixed stack buffer. Collection
        // still admits that working set before its first backing read.
        let _scratch = store
            .resident_budget()
            .reserve_bytes(CHUNK + 4096, CHUNK + 4096)?;
        store.verify_retained(check)
    }
}
impl Drop for ParkedSource {
    fn drop(&mut self) {
        let Some(store) = self.store.take() else {
            return;
        };
        let Some(custody) = self.custody.as_ref() else {
            // Unselected inspection never creates resumable source authority.
            // Private handles/index close on this off-lock worker only.
            drop(store);
            return;
        };
        // No Store/Snapshot view escapes this nominal owner. Taking the Arc
        // atomically prevents weak resumption from creating a second owner.
        let inner =
            Arc::try_unwrap(store.0).unwrap_or_else(|_| unreachable!("Private parked source"));
        let backing = inner.into_backing();
        let mut state = custody.state.lock().unwrap_or_else(|e| e.into_inner());
        debug_assert!(state.parked.is_none());
        state.parked = Some(backing);
    }
}
