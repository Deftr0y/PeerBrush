//! Retain a last owner and destroy the exact stock-egui image before refunding.
use super::*;
use std::{
    io,
    sync::{atomic::{AtomicBool, Ordering}, Condvar, Mutex},
    thread,
    time::Duration,
};

const MAX_PENDING: usize = 4096;
pub(super) fn image_bytes(pixels: usize) -> Result<usize> {
    pixels
        .checked_mul(std::mem::size_of::<egui::Color32>())
        .and_then(|n| n.checked_add(128))
        .ok_or_else(|| "Native upload footprint overflow".into())
}
struct Pending {
    image: Option<Arc<egui::ColorImage>>,
    _charge: Reservation,
    finished: Arc<AtomicBool>,
}
struct State {
    pending: Vec<Pending>,
    reserved: usize,
    limit: usize,
    owners: usize,
}
struct Queue {
    state: Mutex<State>,
    wake: Condvar,
    _index: Reservation,
}
pub(crate) struct Retirement {
    queue: Arc<Queue>,
    pool: ResidentBudget,
}
impl Retirement {
    pub(crate) fn uses_budget(&self, pool: &ResidentBudget) -> bool {
        self.pool.same_pool(pool)
    }
    /// Start before conversion/upload. No context, source or profile is retained.
    pub(crate) fn new(pool: &ResidentBudget) -> Result<Self> {
        Self::start(pool, MAX_PENDING, |queue| {
            thread::Builder::new()
                .name("peerbrush-image-retirement".into())
                .spawn(move || run(queue))
                .map(|_| ())
        })
    }
    fn start(
        pool: &ResidentBudget,
        limit: usize,
        spawn: impl FnOnce(Arc<Queue>) -> io::Result<()>,
    ) -> Result<Self> {
        if limit == 0 || limit > MAX_PENDING {
            return Err("Invalid native upload retirement bound".into());
        }
        let bytes = limit
            .checked_mul(std::mem::size_of::<Pending>())
            .and_then(|n| n.checked_add(512))
            .ok_or("Upload retirement index overflow")?;
        let charge = pool
            .reserve_bytes(bytes, 4 * 1024 * 1024)
            .map_err(|e| e.to_string())?;
        let mut pending = Vec::new();
        pending
            .try_reserve_exact(limit)
            .map_err(|_| "Upload retirement index allocation refused")?;
        let queue = Arc::new(Queue {
            state: Mutex::new(State {
                pending,
                reserved: 0,
                limit,
                owners: 1,
            }),
            wake: Condvar::new(),
            _index: charge,
        });
        spawn(queue.clone()).map_err(|_| "Native upload retirement worker could not start")?;
        Ok(Self {
            queue,
            pool: pool.clone(),
        })
    }
    pub(super) fn pool(&self) -> &ResidentBudget {
        &self.pool
    }
    pub(super) fn admit(&self, pixels: usize) -> Result<Ticket> {
        let bytes = image_bytes(pixels)?;
        {
            let mut state = self.queue.state.lock().unwrap_or_else(|e| e.into_inner());
            reap(&mut state);
            if state.pending.len() + state.reserved >= state.limit {
                return Err("Native upload retirement slots are full".into());
            }
            state.reserved += 1;
        }
        let mut ticket = Ticket {
            charge: None,
            queue: self.queue.clone(),
            reserved: true,
        };
        ticket.charge = Some(
            self.pool
                .reserve_bytes(bytes, MAX_PIXELS * 4 + 128)
                .map_err(|e| e.to_string())?,
        );
        Ok(ticket)
    }
    #[cfg(test)]
    pub(super) fn collect(&self) {
        reap(&mut self.queue.state.lock().unwrap_or_else(|e| e.into_inner()));
    }
}
impl Clone for Retirement {
    fn clone(&self) -> Self {
        self.queue.state.lock().unwrap_or_else(|e| e.into_inner()).owners += 1;
        Self { queue: self.queue.clone(), pool: self.pool.clone() }
    }
}
impl Drop for Retirement {
    fn drop(&mut self) {
        self.queue.state.lock().unwrap_or_else(|e| e.into_inner()).owners -= 1;
        self.queue.wake.notify_all();
    }
}
pub(super) struct Ticket {
    charge: Option<Reservation>,
    queue: Arc<Queue>,
    reserved: bool,
}
impl Ticket {
    pub(super) fn track(mut self, image: &Arc<egui::ColorImage>) -> Fence {
        let finished = Arc::new(AtomicBool::new(false));
        {
            let mut state = self.queue.state.lock().unwrap_or_else(|e| e.into_inner());
            debug_assert!(self.reserved && state.reserved > 0);
            debug_assert!(state.pending.len() < state.pending.capacity());
            state.reserved -= 1;
            state.pending.push(Pending {
                image: Some(image.clone()),
                _charge: self.charge.take().unwrap(),
                finished: finished.clone(),
            });
            self.reserved = false;
        }
        self.queue.wake.notify_all();
        Fence { queue: self.queue.clone(), finished }
    }
}
pub(super) struct Fence {
    queue: Arc<Queue>,
    finished: Arc<AtomicBool>,
}
impl Fence {
    /// Worker-only backpressure. Cancellation remains observable while a
    /// minimized or busy renderer retains its actual image/delta owners.
    pub(super) fn wait(&self, check: &mut dyn FnMut() -> Result<()>) -> Result<()> {
        loop {
            check()?;
            let mut state = self.queue.state.lock().unwrap_or_else(|e| e.into_inner());
            reap(&mut state);
            if self.finished.load(Ordering::Acquire) {
                drop(state);
                return check();
            }
            let _guard = self.queue.wake.wait_timeout(state, Duration::from_millis(20))
                .unwrap_or_else(|e| e.into_inner());
        }
    }
}
impl Drop for Ticket {
    fn drop(&mut self) {
        if self.reserved {
            let mut state = self.queue.state.lock().unwrap_or_else(|e| e.into_inner());
            debug_assert!(state.reserved > 0);
            state.reserved -= 1;
        }
        self.queue.wake.notify_all();
    }
}
fn reap(state: &mut State) {
    let mut i = 0;
    while i < state.pending.len() {
        if Arc::strong_count(state.pending[i].image.as_ref().unwrap()) != 1 {
            i += 1;
            continue;
        }
        let image = state.pending[i].image.take().unwrap();
        // A foreign Weak may upgrade concurrently after the count observation.
        // try_unwrap atomically wins sole ownership or returns the intact Arc.
        match Arc::try_unwrap(image) {
            Ok(image) => {
                // This proves Vec<Color32>'s actual destructor completed before
                // its allocation charge is freed; weak-count polling cannot.
                drop(image);
                let pending = state.pending.swap_remove(i);
                let finished = pending.finished.clone();
                drop(pending);
                finished.store(true, Ordering::Release);
            }
            Err(image) => {
                state.pending[i].image = Some(image);
                i += 1;
            }
        }
    }
}
fn run(queue: Arc<Queue>) {
    loop {
        let mut state = queue.state.lock().unwrap_or_else(|e| e.into_inner());
        reap(&mut state);
        if state.pending.is_empty() {
            if state.reserved == 0 && state.owners == 0 {
                break;
            }
            let _guard = queue.wake.wait(state).unwrap_or_else(|e| e.into_inner());
        } else {
            // egui image owners can drop on another thread without a queue notification.
            let _guard = queue
                .wake
                .wait_timeout(state, Duration::from_millis(20))
                .unwrap_or_else(|e| e.into_inner());
        }
    }
}

#[cfg(test)]
pub(super) fn refused_start(pool: &ResidentBudget) -> Result<Retirement> {
    Retirement::start(pool, 2, |_| {
        Err(io::Error::other("Synthetic thread refusal"))
    })
}
#[cfg(test)]
pub(super) fn small(pool: &ResidentBudget, limit: usize) -> Result<Retirement> {
    Retirement::start(pool, limit, |queue| {
        thread::Builder::new()
            .name("peerbrush-test-image-retirement".into())
            .spawn(move || run(queue))
            .map(|_| ())
    })
}
#[cfg(test)]
pub(super) fn paused(pool: &ResidentBudget) -> Result<(Retirement, std::sync::mpsc::Sender<()>)> {
    let (tx, rx) = std::sync::mpsc::channel();
    let owner = Retirement::start(pool, 8, move |queue| {
        thread::Builder::new()
            .name("peerbrush-test-image-retirement-gate".into())
            .spawn(move || {
                let _ = rx.recv();
                run(queue)
            })
            .map(|_| ())
    })?;
    Ok((owner, tx))
}
