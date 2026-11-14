//! One worker endpoint and one actually published committed baseline, all display-only.
use super::*;
use crate::large_document::{DerivedCache, Reservation, ResidentBudget};
use std::sync::Weak;

#[derive(Clone)]
pub(super) struct Entry {
    pub(super) stamp: Arc<Stamp>,
    pub(super) pixels: Arc<Baseline>,
}
impl Entry {
    fn valid(&self) -> bool {
        let key = self.pixels.tag().key();
        key.source == self.stamp.source
            && key.identity.project_id == self.stamp.project
            && key.identity.document_id == self.stamp.document
            && key.identity.revision == self.stamp.revision
            && key.target == self.stamp.target
            && self.pixels.tag().matches_region(self.stamp.rect)
            && key.mask == self.stamp.mask
            && key.selection_view.is_none()
            && self.stamp.selection_view.is_none()
            && self.stamp.proposal.is_none()
            && self.pixels.tag().matches_edge(self.stamp.edge)
    }
}
#[derive(Default)]
struct State {
    generation: u64,
    endpoint: Option<Entry>,
    baseline: Option<Entry>,
    baseline_tag: Option<native_preview::Tag>,
    registration: Option<Arc<Eviction>>,
    source: Option<DocumentKey>,
    suspended: Option<DocumentKey>,
    epoch: Weak<AtomicU64>,
}
struct Eviction {
    state: Weak<Mutex<State>>,
    budget: ResidentBudget,
    _charge: Reservation,
}
impl DerivedCache for Eviction {
    fn evict(&self) {
        let Some(owner) = self.state.upgrade() else {
            return;
        };
        let Ok(mut state) = owner.try_lock() else {
            return;
        };
        if state
            .registration
            .as_ref()
            .is_none_or(|entry| !std::ptr::eq(entry.as_ref(), self))
        {
            return;
        }
        state.generation = state.generation.wrapping_add(1);
        state.suspended = state.source;
        if let Some(epoch) = state.epoch.upgrade() {
            epoch.fetch_add(1, Ordering::Relaxed);
        }
        let retired = (
            state.endpoint.take(),
            state.baseline.take(),
            state.baseline_tag.take(),
            state.registration.take(),
        );
        drop(state);
        drop(retired);
    }
}
#[derive(Clone, Default)]
pub(super) struct Endpoint(Arc<Mutex<State>>);
impl Endpoint {
    pub(super) fn watch(&self, source: DocumentKey, epoch: &Arc<AtomicU64>) {
        let mut state = self.0.lock().unwrap_or_else(|e| e.into_inner());
        state.source = Some(source);
        state.epoch = Arc::downgrade(epoch);
    }
    pub(super) fn cache_allowed(&self, source: DocumentKey) -> bool {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).suspended != Some(source)
    }
    pub(super) fn bind(&self, budget: &ResidentBudget, source: DocumentKey) -> bool {
        let mut state = self.0.lock().unwrap_or_else(|e| e.into_inner());
        if state.suspended == Some(source) {
            return false;
        }
        if state
            .registration
            .as_ref()
            .is_some_and(|entry| entry.budget.same_pool(budget))
        {
            return true;
        }
        let retired = if state.registration.is_some() {
            state.generation = state.generation.wrapping_add(1);
            Some((
                state.endpoint.take(),
                state.baseline.take(),
                state.baseline_tag.take(),
                state.registration.take(),
            ))
        } else {
            None
        };
        let generation = state.generation;
        drop(state);
        drop(retired);
        let Ok(charge) = budget.reserve_bytes(512, 512) else {
            return false;
        };
        let entry = Arc::new(Eviction {
            state: Arc::downgrade(&self.0),
            budget: budget.clone(),
            _charge: charge,
        });
        let callback: Arc<dyn DerivedCache> = entry.clone();
        if !budget.register_cache(&callback) {
            return false;
        }
        let mut state = self.0.lock().unwrap_or_else(|e| e.into_inner());
        if state.generation != generation || state.suspended == Some(source) {
            return false;
        }
        state.registration = Some(entry);
        state.source = Some(source);
        true
    }
    pub(super) fn fade_baseline(
        &self,
        before: &NativeDocument,
        project: &str,
        target: Option<&str>,
        mask: bool,
    ) -> Option<(u32, Arc<Baseline>, u64)> {
        let state = self.0.lock().unwrap_or_else(|e| e.into_inner());
        let entry = state.baseline.as_ref()?;
        (entry.valid()
            && entry.stamp.matches(project, before)
            && entry.stamp.target.as_deref() == target
            && entry.stamp.mask == mask)
            .then(|| (entry.stamp.edge, entry.pixels.clone(), state.generation))
    }
    pub(super) fn observe_canvas(&self, painted: Option<&native_preview::Tag>) {
        let mut state = self.0.lock().unwrap_or_else(|e| e.into_inner());
        let retired = if state.baseline_tag.as_ref().is_some_and(|expected| {
            painted.is_none_or(|painted| !expected.same_allocation(painted))
        }) {
            Some((state.baseline.take(), state.baseline_tag.take()))
        } else {
            None
        };
        drop(state);
        drop(retired);
    }
    pub(super) fn published(&self, receipt: &mut Receipt, epoch: &AtomicU64) {
        let mut state = self.0.lock().unwrap_or_else(|e| e.into_inner());
        if epoch.load(Ordering::Relaxed) != receipt.job
            || state.suspended == Some(receipt.stamp.source)
            || state.registration.is_none()
        {
            let retired = receipt.baseline.take();
            drop(state);
            drop(retired);
            return;
        }
        let old = if let Some(pixels) = receipt.baseline.take() {
            let old = state.baseline.replace(Entry {
                stamp: receipt.stamp.clone(),
                pixels,
            });
            state.baseline_tag = Some(receipt.tag.clone());
            old
        } else if receipt.stamp.blend {
            state.baseline_tag = Some(receipt.tag.clone());
            None
        } else {
            state.baseline_tag = None;
            state.baseline.take()
        };
        drop(state);
        drop(old);
    }
    pub(super) fn generation(&self) -> u64 {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).generation
    }
    pub(super) fn get(&self, stamp: &Stamp) -> Option<Arc<Baseline>> {
        let mut state = self.0.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(entry) = &state.endpoint {
            if entry.valid() && entry.stamp.same_view(stamp) {
                return Some(entry.pixels.clone());
            }
        }
        let old = state.endpoint.take();
        drop(state);
        drop(old);
        None
    }
    pub(super) fn pan_baseline(&self, stamp: &Stamp) -> Option<Arc<Baseline>> {
        let state = self.0.lock().unwrap_or_else(|e| e.into_inner());
        let entry = state.baseline.as_ref()?;
        (entry.valid() && entry.stamp.same_display_source(stamp) && entry.stamp.rect != stamp.rect)
            .then(|| entry.pixels.clone())
    }
    pub(super) fn store(
        &self,
        generation: u64,
        entry: Entry,
        check: &dyn Fn() -> Result<()>,
    ) -> Result<()> {
        check()?;
        if !entry.valid() {
            return Err("Native canvas endpoint differs from its captured source/view".into());
        }
        let mut state = self.0.lock().unwrap_or_else(|e| e.into_inner());
        check()?;
        if state.generation != generation {
            return Err("Native canvas endpoint was retired before completion".into());
        }
        let old = state.endpoint.replace(entry);
        drop(state);
        drop(old);
        Ok(())
    }
    pub(super) fn clear(&self) -> bool {
        let mut state = self.0.lock().unwrap_or_else(|e| e.into_inner());
        state.generation = state.generation.wrapping_add(1);
        let retired = state.endpoint.is_some() || state.baseline.is_some();
        let old = (
            state.endpoint.take(),
            state.baseline.take(),
            state.baseline_tag.take(),
            state.registration.take(),
        );
        drop(state);
        drop(old);
        retired
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn busy_cache_refuses_without_waiting_and_retired_callbacks_cannot_cancel_new_jobs() {
        let fixture = crate::engine::native::worker::tests::Fixture::new();
        let (runtime, _connection) = fixture.runtime();
        let pool = runtime.resident();
        let document =
            NativeDocument::new_blank(2, 3, 16, &pool, &runtime.disk(), &|| Ok(())).unwrap();
        let endpoint = Endpoint::default();
        let epoch = Arc::new(AtomicU64::new(7));
        endpoint.watch(document.key(), &epoch);
        assert!(endpoint.bind(&pool, document.key()));
        let mandatory = pool
            .reserve_bytes(pool.limit() - pool.used(), pool.limit())
            .unwrap();
        let busy = endpoint.0.lock().unwrap();
        assert!(pool.reserve_bytes(1, 1).is_err());
        assert_eq!(epoch.load(Ordering::Relaxed), 7);
        drop(busy);
        drop(pool.reserve_bytes(1, 1).unwrap());
        assert_eq!(epoch.load(Ordering::Relaxed), 8);
        drop(mandatory);
        let next = NativeDocument::new_blank(2, 3, 16, &pool, &runtime.disk(), &|| Ok(())).unwrap();
        endpoint.watch(next.key(), &epoch);
        assert!(endpoint.bind(&pool, next.key()));
        let old = endpoint.0.lock().unwrap().registration.clone().unwrap();
        let generation = endpoint.generation();
        endpoint.clear();
        assert!(endpoint.bind(&pool, next.key()));
        old.evict();
        assert_eq!(epoch.load(Ordering::Relaxed), 8);
        assert_eq!(endpoint.generation(), generation + 1);
        assert!(endpoint.cache_allowed(next.key()));
        let weak = Arc::downgrade(&endpoint.0);
        drop(endpoint);
        assert!(
            weak.upgrade().is_none(),
            "Budget registry must not retain cache owners"
        );
    }
}
