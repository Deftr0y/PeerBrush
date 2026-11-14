//! Actual proposal sources live in the workspace, never in Engine records.
//! Engine bindings invalidate through atomics; source retirement runs off-lock.
use super::*;
use crate::{
    collaboration::{Proposal, MAX_PENDING, MAX_RECORDS},
    engine::{self, authority::NativeSeed, view::DocumentView, Engine, RuntimeValue},
    large_document::ResidentBudget,
    psd::memory36::MetadataCharge,
    server::{EditControl, EditReply, FileOperation, Shared},
    workspace::{self, native_dispatch},
};
use serde::{ser::SerializeSeq, Serializer};
use serde_json::Value;
use std::sync::{
    atomic::{AtomicBool, AtomicU64, Ordering},
    Mutex, OnceLock,
};

const CAPACITY: usize = MAX_PENDING * workspace::MAX_PROJECTS;
struct Signal {
    live: AtomicBool,
    source: native::DocumentKey,
    expires: AtomicU64,
    project: String,
    canvas_size: OnceLock<[u32; 2]>,
}
/// This is the sole Engine owner. Cloned review/approval signals do not delay
/// invalidation when its record stops, is evicted or its Engine is dropped.
pub(crate) struct Binding(Arc<Signal>);
impl Drop for Binding {
    fn drop(&mut self) {
        self.0.live.store(false, Ordering::Release);
    }
}
impl Binding {
    pub(crate) fn matches(&self, view: DocumentView<'_>, project: &str) -> bool {
        self.0.live.load(Ordering::Acquire)
            && self.0.project == project
            && view.native().is_some_and(|doc| doc.key() == self.0.source)
    }
}
#[derive(Clone)]
pub(crate) struct Approval {
    signal: Arc<Signal>,
}
impl Approval {
    pub(crate) fn same(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.signal, &other.signal)
    }
    pub(crate) fn canvas_size(
        &self,
        project: &str,
        source: native::DocumentKey,
    ) -> Result<[u32; 2]> {
        self.active()?;
        if self.signal.project != project || self.signal.source != source {
            return Err("Proposal canvas belongs to another project revision".into());
        }
        self.signal
            .canvas_size
            .get()
            .copied()
            .ok_or_else(|| "Proposal canvas is not ready".into())
    }
    pub(crate) fn check(&self, shared: &Shared) -> Result<()> {
        let mut engine = native_dispatch::try_source_engine(shared)?;
        self.validate(&mut engine).map(|_| ())
    }
    pub(super) fn validate(&self, engine: &mut Engine) -> Result<usize> {
        engine.expire();
        self.active()?;
        engine
            .proposals
            .iter()
            .position(|p| {
                p.status == "pending"
                    && p.native
                        .as_ref()
                        .is_some_and(|binding| Arc::ptr_eq(&binding.0, &self.signal))
            })
            .ok_or_else(|| "Proposal is no longer pending in this project".into())
    }
    pub(super) fn accepted(&self, engine: &mut Engine, position: usize) {
        engine.proposals[position].stop("accepted", "Accepted by the human as one undoable edit.");
        engine.expire_proposals();
    }
    fn active(&self) -> Result<()> {
        if self.signal.live.load(Ordering::Acquire)
            && engine::now() < self.signal.expires.load(Ordering::Acquire)
        {
            Ok(())
        } else {
            Err("Proposal ended or expired; current work preserved".into())
        }
    }
}
enum Payload {
    Preparing,
    Empty,
    Held(Arc<Mutex<Option<Draft>>>),
    Failed(CaptureFailure),
    Retiring(Retirement),
    InFlight,
}
struct Slot {
    signal: Arc<Signal>,
    payload: Payload,
    _charge: Arc<MetadataCharge>,
}
pub(crate) struct Store {
    slots: Vec<Slot>,
    pool: ResidentBudget,
    warning: Option<&'static str>,
    closed: bool,
    _charge: Arc<MetadataCharge>,
}
impl Store {
    fn new(pool: &ResidentBudget) -> Result<Self> {
        let charge =
            MetadataCharge::reserve(pool, CAPACITY * std::mem::size_of::<Slot>() + 1024, &|| {
                Ok(())
            })?;
        let mut slots = Vec::new();
        slots
            .try_reserve_exact(CAPACITY)
            .map_err(|_| "Proposal slot allocation refused")?;
        if slots.capacity() > CAPACITY {
            return Err("Proposal slot capacity exceeds admission".into());
        }
        Ok(Self {
            slots,
            pool: pool.clone(),
            warning: None,
            closed: false,
            _charge: charge,
        })
    }
}
/// A preparing slot also consumes the per-project limit. Its destructor only
/// invalidates; source cleanup remains an explicit worker operation.
struct Ticket {
    store: Arc<Mutex<Store>>,
    signal: Arc<Signal>,
    published: bool,
}
impl Drop for Ticket {
    fn drop(&mut self) {
        if !self.published {
            self.signal.live.store(false, Ordering::Release);
            let mut state = self
                .store
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if let Some(slot) = state
                .slots
                .iter_mut()
                .find(|slot| Arc::ptr_eq(&slot.signal, &self.signal))
            {
                if matches!(slot.payload, Payload::Preparing) {
                    slot.payload = Payload::Empty;
                }
            }
        }
    }
}
fn configured(shared: &Shared) -> Result<(Arc<Mutex<Store>>, NativeSeed)> {
    let runtime = native_dispatch::runtime_for(shared)?;
    let (registry, seed) = {
        let engine = shared.lock().unwrap();
        (
            engine
                .workspace
                .as_ref()
                .and_then(std::sync::Weak::upgrade)
                .ok_or("Proposal workspace unavailable")?,
            engine
                .native_seed()?
                .ok_or("Proposal source is no longer native")?,
        )
    };
    if !runtime.resident().same_pool(seed.document().budget())
        || !runtime.disk().same_pool(seed.document().disk_budget())
    {
        return Err("Proposal source belongs to another configured service".into());
    }
    let existing = {
        let state = registry.lock().unwrap();
        if state.native_proposals_closing {
            return Err("Proposal source service is closing".into());
        }
        state.native_proposals.clone()
    };
    if let Some(store) = existing {
        if !store
            .lock()
            .unwrap()
            .pool
            .same_pool(seed.document().budget())
        {
            return Err("Proposal storage belongs to another source service".into());
        }
        return Ok((store, seed));
    }
    let candidate = Arc::new(Mutex::new(Store::new(seed.document().budget())?));
    let store = {
        let mut workspace = registry.lock().unwrap();
        if workspace.exiting
            || workspace.native_proposals_closing
            || !workspace
                .source_runtime
                .as_ref()
                .is_some_and(|current| Arc::ptr_eq(current, &runtime))
        {
            return Err("Proposal source service changed".into());
        }
        workspace.native_proposals.get_or_insert(candidate).clone()
    };
    Ok((store, seed))
}
fn reserve(store: Arc<Mutex<Store>>, seed: &NativeSeed) -> Result<Ticket> {
    let charge = MetadataCharge::reserve(
        seed.document().budget(),
        std::mem::size_of::<Draft>() + seed.project_id().len() + 2048,
        &|| Ok(()),
    )?;
    let signal = Arc::new(Signal {
        live: AtomicBool::new(true),
        source: seed.document().key(),
        expires: AtomicU64::new(u64::MAX),
        project: seed.project_id().into(),
        canvas_size: OnceLock::new(),
    });
    {
        let mut state = store.lock().unwrap();
        if state.closed {
            return Err("Proposal source service is closing".into());
        }
        if state.slots.len() == CAPACITY
            || state
                .slots
                .iter()
                .filter(|slot| slot.signal.project == seed.project_id())
                .count()
                >= MAX_PENDING
        {
            return Err("Four native proposals or their cleanup owners are pending. Review or finish cleanup first".into());
        }
        state.slots.push(Slot {
            signal: signal.clone(),
            payload: Payload::Preparing,
            _charge: charge,
        });
    }
    Ok(Ticket {
        store,
        signal,
        published: false,
    })
}
impl Ticket {
    fn install(&self, payload: Payload) -> std::result::Result<(), Payload> {
        let mut state = self.store.lock().unwrap();
        if state.closed {
            return Err(payload);
        }
        let Some(slot) = state
            .slots
            .iter_mut()
            .find(|slot| Arc::ptr_eq(&slot.signal, &self.signal))
        else {
            return Err(payload);
        };
        debug_assert!(matches!(slot.payload, Payload::Preparing));
        slot.payload = payload;
        Ok(())
    }
}

#[derive(Serialize)]
struct Metadata<'a> {
    id: &'a str,
    actor: &'a str,
    label: &'a str,
    task: Option<&'a str>,
    document_id: &'a str,
    source_revision: u64,
    commands: &'a [Value],
    scopes: &'a [engine::Scope],
    expires: u64,
    status: &'a str,
    reason: &'a str,
    created: Created<'a>,
    operations: Operations<'a>,
}
struct Created<'a>(&'a PreparedNative);
impl Serialize for Created<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        let mut seq = serializer.serialize_seq(None)?;
        for layer in self.0.created_layers() {
            seq.serialize_element(layer)?;
        }
        seq.end()
    }
}
struct Operations<'a>(&'a native::CommandDescriptor);
impl Serialize for Operations<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        let mut seq = serializer.serialize_seq(Some(self.0.commands().len()))?;
        for command in self.0.commands() {
            seq.serialize_element(&command.parameters()["op"])?;
        }
        seq.end()
    }
}
fn record(draft: &Draft, signal: Arc<Signal>) -> Result<Proposal> {
    let descriptor = draft.prepared.shared_descriptor()?;
    let candidate = draft.prepared.preview().metadata();
    signal
        .canvas_size
        .set([candidate.width(), candidate.height()])
        .map_err(|_| "Proposal canvas was already bound")?;
    let id = engine::id();
    let expires = engine::now().saturating_add(900);
    signal.expires.store(expires, Ordering::Release);
    let record = Metadata {
        id: &id,
        actor: &draft.attribution.actor,
        label: &draft.attribution.label,
        task: draft.attribution.task.as_deref(),
        document_id: &draft.scene.identity().document_id,
        source_revision: draft.scene.identity().revision,
        commands: &[],
        scopes: draft.prepared.scopes(),
        expires,
        status: "pending",
        reason: "",
        created: Created(&draft.prepared),
        operations: Operations(&descriptor),
    };
    native::project_admitted(
        &record,
        draft.prepared.base().budget(),
        true,
        &|| Ok(()),
        |mut proposal: Proposal, owner| {
            proposal.native = Some(Binding(signal));
            proposal.native_charge = Some(owner);
            proposal
        },
    )
}

pub(crate) fn create_stored(
    shared: &Shared,
    actor: &str,
    commands: &[Value],
    document: &str,
    revision: u64,
    label: &str,
    task: Option<&str>,
) -> Result<RuntimeValue<Value>> {
    let _operation = FileOperation::begin(shared)?;
    let (store, seed) = configured(shared)?;
    shared.lock().unwrap().expire();
    let _ = maintain(&store);
    let mut ticket = reserve(store.clone(), &seed)?;
    let mut draft = match Draft::prepare_shared(
        shared,
        actor,
        commands,
        document,
        revision,
        label,
        task,
        &|| Ok(()),
    ) {
        Ok(draft) => draft,
        Err(refused) => {
            let reason = refused.reason().to_owned();
            if let Err(payload) = ticket.install(Payload::Failed(refused)) {
                let _ = retire(payload);
            }
            drop(ticket);
            let _ = maintain(&store);
            return Err(reason);
        }
    };
    if let Err(reason) = draft.park_activity() {
        if let Err(payload) = ticket.install(Payload::Held(Arc::new(Mutex::new(Some(draft))))) {
            let _ = retire(payload);
        }
        drop(ticket);
        let _ = maintain(&store);
        return Err(reason);
    }
    let prepared = (|| {
        let proposal = record(&draft, ticket.signal.clone())?;
        let state =
            RuntimeValue::project(&proposal.wire(), draft.prepared.base().budget(), &|| Ok(()))?;
        Ok((proposal, state))
    })();
    let owner = Arc::new(Mutex::new(Some(draft)));
    if let Err(payload) = ticket.install(Payload::Held(owner)) {
        drop(prepared);
        drop(ticket);
        let _ = retire(payload);
        return Err(
            "Proposal source service closed before publication; current work preserved".into(),
        );
    }
    let result = match prepared {
        Err(reason) => Err(reason),
        Ok((proposal, state)) => {
            let result = shared.lock().unwrap().queue_native_proposal(proposal);
            result.map(|_| state)
        }
    };
    ticket.published = result.is_ok();
    drop(ticket);
    if result.is_err() {
        let _ = maintain(&store);
    }
    result
}

fn observed(
    shared: &Shared,
    id: &str,
) -> Result<(Arc<Mutex<Store>>, Approval, Arc<Mutex<Option<Draft>>>)> {
    let (store, _) = configured(shared)?;
    let signal = {
        let mut engine = shared.lock().unwrap();
        engine.expire();
        engine
            .proposals
            .iter()
            .find(|p| p.id == id)
            .and_then(|p| p.native.as_ref())
            .map(|binding| binding.0.clone())
            .ok_or("Native proposal is not pending in this project")?
    };
    let owner = {
        let state = store.lock().unwrap();
        let slot = state
            .slots
            .iter()
            .find(|slot| Arc::ptr_eq(&slot.signal, &signal))
            .ok_or("Native proposal owner unavailable")?;
        match &slot.payload {
            Payload::Held(owner) => owner.clone(),
            _ => return Err("Native proposal is still preparing or retiring".into()),
        }
    };
    Ok((store, Approval { signal }, owner))
}
pub(crate) fn review_guard(shared: &Shared, id: &str) -> Result<Approval> {
    let mut engine = native_dispatch::try_source_engine(shared)?;
    engine.ensure_open()?;
    engine.expire();
    let signal = engine
        .proposals
        .iter()
        .find(|p| p.id == id)
        .and_then(|p| p.native.as_ref())
        .map(|binding| binding.0.clone())
        .ok_or("Native proposal is not pending in this project")?;
    Ok(Approval { signal })
}
pub(crate) fn review_stored(
    shared: &Shared,
    id: &str,
    rect: Option<[i32; 4]>,
    edge: u32,
    target: Option<&str>,
    mask: bool,
    selection_view: Option<&str>,
    check: &dyn Fn() -> Result<()>,
) -> Result<TransientFrame> {
    review_stored_with(shared, id, rect, edge, target, mask, selection_view, check, false,
        |request, check| request.render(&mut || check()))
}
pub(crate) fn review_stored_with<T: DisplayOutput>(
    shared: &Shared,
    id: &str,
    rect: Option<[i32; 4]>,
    edge: u32,
    target: Option<&str>,
    mask: bool,
    selection_view: Option<&str>,
    check: &dyn Fn() -> Result<()>,
    canvas: bool,
    render: impl FnOnce(&Request, &dyn Fn() -> Result<()>) -> Result<T>,
) -> Result<TransientFrame<T>> {
    let (_store, approval, owner) = observed(shared, id)?;
    let draft = owner
        .try_lock()
        .map_err(|_| "Proposal is being accepted; review deferred")?;
    approval.active()?;
    let frame = draft
        .as_ref()
        .ok_or("Proposal acceptance is pending")?
        .render_shared_with(shared, rect, edge, target, mask, selection_view, &|| {
            check()?;
            approval.active()
        }, canvas, render)?;
    approval.active()?;
    Ok(frame)
}
pub(crate) fn accept_stored(
    shared: &Shared,
    actor: &str,
    id: &str,
    document: &str,
    revision: u64,
    control: &EditControl,
) -> Result<EditReply> {
    let (store, approval, owner) = observed(shared, id)?;
    let result = {
        let mut held = owner
            .try_lock()
            .map_err(|_| "Proposal review is still rendering; acceptance deferred")?;
        let draft = held
            .take()
            .ok_or("Proposal acceptance is already pending")?;
        match draft.accept_guarded(
            shared,
            actor,
            document,
            revision,
            control,
            &approval,
            &|| approval.active(),
        ) {
            Ok(reply) => Ok(reply),
            Err(refused) => {
                let reason = refused.reason.clone();
                *held = Some(refused.into_draft());
                Err(reason)
            }
        }
    };
    drop(owner);
    let _ = maintain(&store);
    result
}

fn retire(payload: Payload) -> std::result::Result<(), Payload> {
    let retirement = match payload {
        Payload::Empty => return Ok(()),
        Payload::Preparing => return Err(Payload::Preparing),
        Payload::Held(owner) => {
            let held = match Arc::try_unwrap(owner) {
                Ok(held) => held,
                Err(owner) => return Err(Payload::Held(owner)),
            };
            let Some(draft) = held
                .into_inner()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
            else {
                return Ok(());
            };
            match draft.prepare_retirement() {
                Ok(retirement) => retirement,
                Err(refused) => {
                    return Err(Payload::Held(Arc::new(Mutex::new(Some(
                        refused.into_draft(),
                    )))))
                }
            }
        }
        Payload::Failed(refused) => match refused.prepare_retirement() {
            Ok(Some(retirement)) => retirement,
            Ok(None) => return Ok(()),
            Err((refused, _)) => return Err(Payload::Failed(refused)),
        },
        Payload::Retiring(retirement) => retirement,
        Payload::InFlight => return Err(Payload::InFlight),
    };
    if !retirement.retryable() {
        return Err(Payload::Retiring(retirement));
    }
    retirement
        .retire()
        .map_err(|refused| Payload::Retiring(refused.into_retirement()))
}
pub(crate) fn maintain(store: &Arc<Mutex<Store>>) -> Result<()> {
    let mut signals: [Option<Arc<Signal>>; CAPACITY] = std::array::from_fn(|_| None);
    {
        let state = store.lock().unwrap();
        for (index, slot) in state
            .slots
            .iter()
            .filter(|slot| !slot.signal.live.load(Ordering::Acquire))
            .enumerate()
        {
            signals[index] = Some(slot.signal.clone());
        }
    }
    for signal in signals.into_iter().flatten() {
        let payload = {
            let mut state = store.lock().unwrap();
            let Some(slot) = state
                .slots
                .iter_mut()
                .find(|slot| Arc::ptr_eq(&slot.signal, &signal))
            else {
                continue;
            };
            if matches!(slot.payload, Payload::InFlight | Payload::Preparing) {
                continue;
            }
            std::mem::replace(&mut slot.payload, Payload::InFlight)
        };
        let retired = retire(payload);
        let mut state = store.lock().unwrap();
        let Some(index) = state
            .slots
            .iter()
            .position(|slot| Arc::ptr_eq(&slot.signal, &signal))
        else {
            drop(state);
            drop(retired);
            continue;
        };
        match retired {
            Ok(()) => {
                state.slots.swap_remove(index);
            }
            Err(payload) => {
                state.slots[index].payload = payload;
            }
        }
    }
    let mut state = store.lock().unwrap();
    state.warning = state
        .slots
        .iter()
        .any(|slot| !slot.signal.live.load(Ordering::Acquire))
        .then_some("Native proposal cleanup is pending; source quota remains reserved");
    state.warning.map_or(Ok(()), |warning| Err(warning.into()))
}
// Teardown never verifies or unlinks implicitly. Unsettled sources remain in
// actual runtime custody and startup census after the final store owner.

pub(crate) fn maintain_workspace(workspace: &workspace::Registry) {
    for shared in workspace::handles_in(workspace) {
        if let Ok(mut engine) = shared.try_lock() {
            engine.expire();
        }
    }
    let store = workspace.lock().unwrap().native_proposals.clone();
    if let Some(store) = store {
        let _ = maintain(&store);
    }
}

pub(crate) fn finish_workspace(workspace: &workspace::Registry) {
    let store = {
        let mut state = workspace
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state.native_proposals_closing = true;
        state.native_proposals.take()
    };
    let Some(store) = store else {
        return;
    };
    let pending = {
        let mut state = store
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state.closed = true;
        for slot in &state.slots {
            slot.signal.live.store(false, Ordering::Release);
        }
        std::mem::take(&mut state.slots)
    };
    for shared in workspace::handles_in(workspace) {
        if let Ok(mut engine) = shared.try_lock() {
            for proposal in &mut engine.proposals {
                if proposal.native.is_some() {
                    proposal.stop(
                        "interrupted",
                        "Source service closed; current work preserved.",
                    );
                }
            }
        }
    }
    // Release every idle session before instance drain. Active workers retain
    // their own owners and observe closing; refused files keep actual custody.
    for slot in pending {
        let _ = retire(slot.payload);
    }
}

impl Engine {
    fn queue_native_proposal(&mut self, proposal: Proposal) -> Result<()> {
        self.expire();
        let view = engine::view::current(self);
        if proposal.document != view.id()
            || proposal.revision != view.revision()
            || !proposal
                .native
                .as_ref()
                .is_some_and(|binding| binding.matches(view, &self.project_id))
        {
            return Err("The proposal source changed; current work preserved".into());
        }
        self.check(&proposal.actor, &proposal.scopes)?;
        super::capture::check_task(self, &proposal.actor, proposal.task.as_deref())?;
        if self.proposals.iter().filter(|p| p.pending()).count() >= MAX_PENDING {
            return Err("Four proposals are already pending. Review or reject one first".into());
        }
        if self.proposals.len() == MAX_RECORDS {
            let at = self
                .proposals
                .iter()
                .position(|p| !p.pending())
                .ok_or("Proposal record capacity is full")?;
            self.proposals.remove(at);
        }
        self.proposals.push(proposal);
        self.status = "AI proposal ready · review before accepting".into();
        Ok(())
    }
}

#[cfg(test)]
mod tests;
