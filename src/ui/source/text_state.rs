//! Private TextEdit histories retain their actual owner until a closed-nonce
//! reaper clears the shared undoers. Font/layout/platform output are separate.
use crate::{large_document::ResidentBudget, psd::memory36::MetadataCharge};
use eframe::egui::{self, text_edit::TextEditState, Context, Id};
use std::{
    io,
    sync::{
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
        Arc, Condvar, Mutex, Weak,
    },
    thread,
};

const SLOTS: usize = 520;
const FIELD_BYTES: usize = crate::svg::MARKUP_LIMIT + 16384 + 512 + 512 * 64;
// Pinned egui:100 undos, redo/flux overlap, bounded event/filter and frame
// copies. Retained platform output/clipboard and layout owners are separate.
const HEADER_BYTES: usize =
    SLOTS * 512 * (std::mem::size_of::<(egui::text::CCursorRange, String)>() + 16);
const BYTES: usize =
    FIELD_BYTES * 128 + HEADER_BYTES + 8192 * std::mem::size_of::<egui::Event>() + 1024 * 1024;
static NEXT: AtomicU64 = AtomicU64::new(1);
const MAX_RETIREMENTS: usize = 16;
static RETIREMENTS: AtomicUsize = AtomicUsize::new(0);
struct Permit;
impl Permit {
    fn new() -> Result<Self, String> {
        RETIREMENTS
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |count| {
                (count < MAX_RETIREMENTS).then_some(count + 1)
            })
            .map_err(|_| "Earlier source editors are still retiring")?;
        Ok(Self)
    }
}
impl Drop for Permit {
    fn drop(&mut self) {
        RETIREMENTS.fetch_sub(1, Ordering::AcqRel);
    }
}
struct Slot {
    id: Id,
    limit: usize,
    state: TextEditState,
}
struct Inner {
    slots: Vec<Slot>,
    bytes: usize,
    acknowledged: bool,
    owner: Option<Arc<MetadataCharge>>,
}
struct Queue {
    context: Context,
    marker: Id,
    closed: AtomicBool,
    inner: Mutex<Inner>,
    wake: Condvar,
    _permit: Permit,
}
pub(super) struct TextState {
    queue: Arc<Queue>,
    nonce: u64,
}
impl TextState {
    pub fn new(context: &Context, pool: &ResidentBudget) -> Result<Self, String> {
        Self::start(context, pool, |queue| {
            thread::Builder::new()
                .name("peerbrush-text-retirement".into())
                .spawn(move || retire(queue))
                .map(|_| ())
        })
    }
    fn start(
        context: &Context,
        pool: &ResidentBudget,
        spawn: impl FnOnce(Arc<Queue>) -> io::Result<()>,
    ) -> Result<Self, String> {
        let nonce = NEXT
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |n| n.checked_add(1))
            .map_err(|_| "Source editor nonce exhausted")?;
        let permit = Permit::new()?;
        let owner = MetadataCharge::reserve(pool, BYTES, &|| Ok(()))?;
        let mut slots = Vec::new();
        slots
            .try_reserve_exact(SLOTS)
            .map_err(|_| "Source text-state admission refused")?;
        if slots.capacity() > SLOTS {
            return Err("Source text-state slot capacity exceeded admission".into());
        }
        let marker = Id::new(("native-source-state-owner", nonce));
        let queue = Arc::new(Queue {
            context: context.clone(),
            marker,
            closed: AtomicBool::new(false),
            inner: Mutex::new(Inner {
                slots,
                bytes: 0,
                acknowledged: false,
                owner: Some(owner),
            }),
            wake: Condvar::new(),
            _permit: permit,
        });
        spawn(queue.clone()).map_err(|_| "Source text retirement worker could not start")?;
        // Construct the Drop guard before publishing the Context marker: even
        // an interrupted Context write closes and wakes the actual owner.
        let result = Self { queue, nonce };
        context.data_mut(|data| data.insert_temp(marker, Arc::downgrade(&result.queue)));
        Ok(result)
    }
    pub fn nonce(&self) -> u64 {
        self.nonce
    }
    pub fn id(&self, name: impl std::hash::Hash) -> Id {
        Id::new(("native-source-field", self.nonce, name))
    }
    fn admitted(&self) -> Result<(), String> {
        if self.queue.closed.load(Ordering::Acquire) {
            return Err("Source text editor is closed".into());
        }
        let marker = self
            .queue
            .context
            .data(|data| data.get_temp::<Weak<Queue>>(self.queue.marker));
        if !marker
            .and_then(|value| value.upgrade())
            .is_some_and(|value| Arc::ptr_eq(&value, &self.queue))
        {
            return Err("Source text-state Context was replaced".into());
        }
        Ok(())
    }
    pub fn prepare(&self, id: Id, limit: usize) -> Result<(), String> {
        self.admitted()?;
        let mut inner = self
            .queue
            .inner
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if self.queue.closed.load(Ordering::Acquire) {
            return Err("Source text editor is closed".into());
        }
        if let Some(slot) = inner.slots.iter().find(|slot| slot.id == id) {
            if slot.limit != limit {
                return Err("Source text field admission changed".into());
            }
            // Never permit TextEdit to default a replacement, unowned undoer.
            if TextEditState::load(&self.queue.context, id).is_none() {
                slot.state.clone().store(&self.queue.context, id);
            }
            return Ok(());
        }
        if inner.slots.len() == SLOTS
            || inner
                .bytes
                .checked_add(limit)
                .is_none_or(|bytes| bytes > FIELD_BYTES)
        {
            return Err("Source text fields exceed their resident admission".into());
        }
        if TextEditState::load(&self.queue.context, id).is_some() {
            return Err("Source text field ID was already owned".into());
        }
        // Seed before any widget invocation, retaining the exact shared Arc in
        // the cleanup owner even if the Context entry is later removed.
        let state = TextEditState::default();
        inner.bytes += limit;
        inner.slots.push(Slot { id, limit, state });
        inner
            .slots
            .last()
            .unwrap()
            .state
            .clone()
            .store(&self.queue.context, id);
        Ok(())
    }
    pub fn observe(&self, id: Id) -> Result<(), String> {
        self.admitted()?;
        let mut inner = self
            .queue
            .inner
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let slot = inner
            .slots
            .iter_mut()
            .find(|slot| slot.id == id)
            .ok_or("Source text field was not admitted")?;
        // Pinned TextEdit loads the seeded State and stores its cheap clone;
        // only this private nonce writes the ID. Keep its current cursor/IME
        // metadata too, while retaining the same shared seeded undoer.
        slot.state = TextEditState::load(&self.queue.context, id)
            .ok_or("Source text field was removed during editing")?;
        Ok(())
    }
    pub fn close(&mut self) {
        self.queue.closed.store(true, Ordering::Release);
        self.queue.wake.notify_one();
    }
    #[cfg(test)]
    fn wait_closed(&self) {
        wait_closed(&self.queue);
    }
}
impl Drop for TextState {
    fn drop(&mut self) {
        self.close();
    }
}
fn retire(queue: Arc<Queue>) {
    let mut inner = queue
        .inner
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    while !queue.closed.load(Ordering::Acquire) {
        // Drop cannot lock Inner while Context/user code is unwinding. A
        // timeout also closes the condition-check/notify race without making
        // Drop block on that mutex.
        inner = queue
            .wake
            .wait_timeout(inner, std::time::Duration::from_millis(20))
            .unwrap_or_else(|error| error.into_inner())
            .0;
    }
    // A closed nonce cannot prepare a future writer. Each retained State clone
    // refers to the actual seeded undoer; clear_undoer synchronously drops all
    // String/deque/redo payloads even when external State clones survive.
    for slot in &mut inner.slots {
        slot.state.clear_undoer();
        queue.context.data_mut(|data| {
            data.remove::<TextEditState>(slot.id);
            data.remove::<String>(slot.id);
        });
        queue
            .context
            .memory_mut(|memory| memory.surrender_focus(slot.id));
    }
    inner.slots = Vec::new();
    queue
        .context
        .data_mut(|data| data.remove::<Weak<Queue>>(queue.marker));
    drop(inner.owner.take());
    inner.acknowledged = true;
    queue.wake.notify_all();
}
#[cfg(test)]
fn wait_closed(queue: &Arc<Queue>) {
    let inner = queue
        .inner
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let (inner, timeout) = queue
        .wake
        .wait_timeout_while(inner, std::time::Duration::from_secs(5), |inner| {
            !inner.acknowledged
        })
        .unwrap_or_else(|error| error.into_inner());
    assert!(
        !timeout.timed_out() && inner.acknowledged,
        "Source undo retirement did not acknowledge cleanup"
    );
}
#[cfg(test)]
mod tests {
    use super::*;
    fn populated(owner: &TextState, id: Id) -> TextEditState {
        owner.prepare(id, 64).unwrap();
        let mut state = TextEditState::load(&owner.queue.context, id).unwrap();
        let mut undo = egui::util::undoer::Undoer::default();
        undo.feed_state(0., &(Default::default(), "before".to_owned()));
        undo.add_undo(&(Default::default(), "after".to_owned()));
        state.set_undoer(undo);
        let clone = state.clone();
        state.store(&owner.queue.context, id);
        owner.observe(id).unwrap();
        clone
    }
    #[test]
    fn private_state_clones_are_cleared_and_only_owned_ids_retire() {
        let pool = ResidentBudget::new(BYTES * 2).unwrap();
        let context = Context::default();
        let mut owner = TextState::new(&context, &pool).unwrap();
        let id = owner.id("text");
        let foreign = Id::new("foreign-text");
        TextEditState::default().store(&context, foreign);
        let clone = populated(&owner, id);
        assert_eq!(pool.used(), BYTES);
        owner.close();
        owner.wait_closed();
        assert_eq!(pool.used(), 0);
        assert!(TextEditState::load(&context, id).is_none());
        assert!(TextEditState::load(&context, foreign).is_some());
        assert!(!clone
            .undoer()
            .has_undo(&(Default::default(), "after".to_owned())));
        assert!(owner.prepare(id, 64).is_err());
        let mut next = TextState::new(&context, &pool).unwrap();
        assert_ne!(id, next.id("text"));
        next.close();
        next.wait_closed();
    }
    #[test]
    fn removed_context_entry_restores_exact_seed_and_unwind_retirement_recovers() {
        let pool = ResidentBudget::new(BYTES * 2).unwrap();
        let context = Context::default();
        let owner = TextState::new(&context, &pool).unwrap();
        let id = owner.id("text");
        let clone = populated(&owner, id);
        context.data_mut(|data| data.remove::<TextEditState>(id));
        owner.prepare(id, 64).unwrap();
        // Clearing the retained clone must clear the restored Context's state.
        let mut retained = clone.clone();
        retained.clear_undoer();
        assert!(!TextEditState::load(&context, id)
            .unwrap()
            .undoer()
            .has_undo(&(Default::default(), "after".to_owned())));
        let clone = populated(&owner, id);
        let queue = owner.queue.clone();
        let interrupted = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
            let _owner = owner;
            panic!("source editor teardown fixture");
        }));
        assert!(interrupted.is_err());
        wait_closed(&queue);
        assert_eq!(pool.used(), 0);
        assert!(!clone
            .undoer()
            .has_undo(&(Default::default(), "after".to_owned())));
    }
    #[test]
    fn replaced_context_refuses_future_writes_and_spawn_or_budget_failure_refunds() {
        let pool = ResidentBudget::new(BYTES * 2).unwrap();
        let context = Context::default();
        let mut owner = TextState::new(&context, &pool).unwrap();
        let id = owner.id("text");
        context.data_mut(|data| data.remove::<Weak<Queue>>(owner.queue.marker));
        assert!(owner.prepare(id, 64).is_err());
        owner.close();
        owner.wait_closed();
        assert_eq!(pool.used(), 0);
        assert!(TextState::start(&context, &pool, |_| Err(io::Error::other(
            "fixture refusal"
        )))
        .is_err());
        assert_eq!(pool.used(), 0);
        let small = ResidentBudget::new(4096).unwrap();
        assert!(TextState::new(&context, &small).is_err());
        assert_eq!(small.used(), 0);
    }
}
