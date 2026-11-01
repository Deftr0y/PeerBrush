//! Own only this editor's private TextEdit undo state. This is not an admission
//! for egui's independent font, layout, or paint caches.
use crate::large_document::{Reservation, ResidentBudget};
use eframe::egui::{self, text_edit::TextEditState, Context, Id};
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc,
};

const SLOTS: usize = 520;
const FIELD_BYTES: usize = crate::svg::MARKUP_LIMIT + 16384 + 512 + 512 * 64;
// Pinned egui 0.31:100 undo states; redo and flux plus the current/previous
// TextEdit Strings can coexist.108 payload copies conservatively covers that
// live set;128 also admits event/filter and per-frame editor copies. Each
// deque/vector can retain its old capacity after undo/redo. Retained platform
// output/clipboard and layout owners require their own admission.
const HEADER_BYTES: usize =
    SLOTS * 512 * (std::mem::size_of::<(egui::text::CCursorRange, String)>() + 16);
const BYTES: usize =
    FIELD_BYTES * 128 + HEADER_BYTES + 8192 * std::mem::size_of::<egui::Event>() + 1024 * 1024;
static NEXT: AtomicU64 = AtomicU64::new(1);

struct Slot {
    id: Id,
    limit: usize,
    state: Option<TextEditState>,
}
struct Inner {
    slots: Vec<Slot>,
    bytes: usize,
    closed: bool,
    acknowledged: bool,
    owner: Option<Reservation>,
}
impl Drop for Inner {
    fn drop(&mut self) {
        // During an interrupted teardown the Context may still expose undoer
        // Arc clones. Uncertain destruction must leave debt, never refund it.
        if !self.acknowledged {
            if let Some(owner) = self.owner.take() {
                std::mem::forget(owner);
            }
        }
    }
}
pub(super) struct TextState {
    context: Context,
    marker: Id,
    nonce: u64,
    inner: Arc<egui::mutex::Mutex<Inner>>,
}
impl TextState {
    pub fn new(context: &Context, pool: &ResidentBudget) -> Result<Self, String> {
        let nonce = NEXT
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |n| n.checked_add(1))
            .map_err(|_| "Source editor nonce exhausted")?;
        let owner = pool
            .reserve_bytes(BYTES, BYTES)
            .map_err(|e| e.to_string())?;
        let mut slots = Vec::new();
        slots
            .try_reserve_exact(SLOTS)
            .map_err(|_| "Source text-state admission refused")?;
        if slots.capacity() > SLOTS {
            return Err("Source text-state slot capacity exceeded admission".into());
        }
        let marker = Id::new(("native-source-state-owner", nonce));
        let inner = Arc::new(egui::mutex::Mutex::new(Inner {
            slots,
            bytes: 0,
            closed: false,
            acknowledged: false,
            owner: Some(owner),
        }));
        context.data_mut(|data| data.insert_temp(marker, inner.clone()));
        Ok(Self {
            context: context.clone(),
            marker,
            nonce,
            inner,
        })
    }
    pub fn nonce(&self) -> u64 {
        self.nonce
    }
    pub fn id(&self, name: impl std::hash::Hash) -> Id {
        Id::new(("native-source-field", self.nonce, name))
    }
    pub fn prepare(&self, id: Id, limit: usize) -> Result<(), String> {
        let marker = self
            .context
            .data(|data| data.get_temp::<Arc<egui::mutex::Mutex<Inner>>>(self.marker));
        if !marker
            .as_ref()
            .is_some_and(|value| Arc::ptr_eq(value, &self.inner))
        {
            return Err("Source text state context was replaced".into());
        }
        let mut inner = self.inner.lock();
        if inner.closed {
            return Err("Source text editor is closed".into());
        }
        if let Some(slot) = inner.slots.iter().find(|slot| slot.id == id) {
            if slot.limit != limit {
                return Err("Source text field admission changed".into());
            }
            if slot.state.is_some() && TextEditState::load(&self.context, id).is_none() {
                return Err("Source text undo state was replaced".into());
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
        if TextEditState::load(&self.context, id).is_some() {
            return Err("Source text field ID was already owned".into());
        }
        inner.bytes += limit;
        inner.slots.push(Slot {
            id,
            limit,
            state: None,
        });
        Ok(())
    }
    pub fn observe(&self, id: Id) -> Result<(), String> {
        let mut inner = self.inner.lock();
        if inner.closed {
            return Err("Source text editor is closed".into());
        }
        let slot = inner
            .slots
            .iter_mut()
            .find(|slot| slot.id == id)
            .ok_or("Source text field was not admitted")?;
        if slot.state.is_none() {
            slot.state = TextEditState::load(&self.context, id);
        }
        Ok(())
    }
    pub fn close(&mut self) {
        let mut inner = self.inner.lock();
        if inner.acknowledged {
            return;
        }
        // This unique nonce is closed before shared undoers are cleared. All
        // source widgets check prepare before writing, including numeric focus.
        inner.closed = true;
        for slot in &mut inner.slots {
            if let Some(state) = &mut slot.state {
                state.clear_undoer();
            }
            if let Some(mut state) = TextEditState::load(&self.context, slot.id) {
                state.clear_undoer();
            }
            self.context.data_mut(|data| {
                data.remove::<TextEditState>(slot.id);
                data.remove::<String>(slot.id);
            });
            self.context
                .memory_mut(|memory| memory.surrender_focus(slot.id));
        }
        inner.slots.clear();
        // Free actual retained slot capacity before releasing its admission.
        inner.slots = Vec::new();
        inner.acknowledged = true;
        drop(inner.owner.take());
        drop(inner);
        self.context
            .data_mut(|data| data.remove::<Arc<egui::mutex::Mutex<Inner>>>(self.marker));
    }
}
impl Drop for TextState {
    fn drop(&mut self) {
        if !std::thread::panicking() {
            self.close();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn private_state_clones_are_cleared_and_only_owned_ids_retire() {
        let pool = ResidentBudget::new(BYTES * 2).unwrap();
        let context = Context::default();
        let mut owner = TextState::new(&context, &pool).unwrap();
        let id = owner.id("text");
        let foreign = Id::new("foreign-text");
        TextEditState::default().store(&context, foreign);
        owner.prepare(id, 64).unwrap();
        let mut state = TextEditState::default();
        let mut undo = egui::util::undoer::Undoer::default();
        undo.feed_state(0., &(Default::default(), "before".to_owned()));
        undo.add_undo(&(Default::default(), "after".to_owned()));
        state.set_undoer(undo);
        let clone = state.clone();
        state.store(&context, id);
        owner.observe(id).unwrap();
        assert!(pool.used() > 0);
        owner.close();
        assert_eq!(pool.used(), 0);
        assert!(TextEditState::load(&context, id).is_none());
        assert!(TextEditState::load(&context, foreign).is_some());
        assert!(!clone
            .undoer()
            .has_undo(&(Default::default(), "after".to_owned())));
        assert!(owner.prepare(id, 64).is_err());
        let next = TextState::new(&context, &pool).unwrap();
        assert_ne!(id, next.id("text"));
    }
    #[test]
    fn context_replacement_refuses_future_writes_and_interrupted_teardown_keeps_debt() {
        let pool = ResidentBudget::new(BYTES * 2).unwrap();
        let context = Context::default();
        let owner = TextState::new(&context, &pool).unwrap();
        let id = owner.id("text");
        context.data_mut(|data| data.remove::<Arc<egui::mutex::Mutex<Inner>>>(owner.marker));
        assert!(owner.prepare(id, 64).is_err());
        drop(owner);
        assert_eq!(pool.used(), 0);
        let mut slots = Vec::new();
        slots.try_reserve_exact(SLOTS).unwrap();
        let debt = pool.reserve_bytes(BYTES, BYTES).unwrap();
        drop(Inner {
            slots,
            bytes: 0,
            closed: true,
            acknowledged: false,
            owner: Some(debt),
        });
        assert_eq!(pool.used(), BYTES);
    }
}
