//! Bounded history-owner capture for worker-side selective compensation.
use super::*;
use ring::digest::{Context, SHA256};

pub(super) struct TruncatedTasks {
    hashes: Vec<[u8; 32]>,
    _charge: Reservation,
}
impl TruncatedTasks {
    pub(super) fn empty(pool: &ResidentBudget) -> Result<Arc<Self>> {
        let charge = pool.reserve_bytes(128, 128).map_err(|e| e.to_string())?;
        Ok(Arc::new(Self {
            hashes: Vec::new(),
            _charge: charge,
        }))
    }
    pub(super) fn contains(&self, owner: &str, task: &str) -> bool {
        self.hashes.binary_search(&task_hash(owner, task)).is_ok()
    }
    pub(super) fn retaining_evicted(
        self: &Arc<Self>,
        record: Option<&Record>,
        pool: &ResidentBudget,
        check: &dyn Fn() -> Result<()>,
    ) -> Result<Arc<Self>> {
        let Some(record) = record else {
            return Ok(self.clone());
        };
        let Some(task) = record.info.task.as_deref() else {
            return Ok(self.clone());
        };
        let hash = task_hash(&record.info.actor, task);
        if self.hashes.binary_search(&hash).is_ok() {
            return Ok(self.clone());
        }
        let count = self
            .hashes
            .len()
            .checked_add(1)
            .ok_or("Native truncated-task count overflow")?;
        let bytes = count
            .checked_mul(32)
            .and_then(|n| n.checked_add(128))
            .ok_or("Native truncated-task footprint overflow")?;
        let charge = pool
            .reserve_bytes(bytes, bytes)
            .map_err(|e| e.to_string())?;
        let mut hashes = Vec::new();
        hashes
            .try_reserve_exact(count)
            .map_err(|_| "Native truncated-task allocation refused")?;
        if hashes.capacity() > count {
            return Err("Native truncated-task allocation exceeds admission".into());
        }
        check()?;
        hashes.extend_from_slice(&self.hashes);
        hashes.push(hash);
        hashes.sort_unstable();
        check()?;
        Ok(Arc::new(Self {
            hashes,
            _charge: charge,
        }))
    }
}
fn task_hash(owner: &str, task: &str) -> [u8; 32] {
    let mut hash = Context::new(&SHA256);
    hash.update(b"PeerBrush native truncated task v1\0");
    for value in [owner, task] {
        hash.update(&(value.len() as u64).to_be_bytes());
        hash.update(value.as_bytes());
    }
    hash.finish().as_ref().try_into().unwrap()
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RequestedTask {
    actor: String,
    owner: String,
    task: String,
}
pub(crate) struct TaskRequest {
    base: DocumentKey,
    data: Admitted<RequestedTask>,
    records: Vec<Arc<Record>>,
    cancellation: Cancellation,
    _charge: Reservation,
}
pub(crate) struct TaskCapture {
    current: NativeDocument,
    request: TaskRequest,
    history: DocumentKey,
    epoch: u64,
}
#[derive(Clone, Copy)]
pub(crate) struct TaskEntry<'a> {
    record: &'a Record,
}
impl<'a> TaskEntry<'a> {
    pub fn before(&self) -> &'a NativeDocument {
        &self.record.before
    }
    pub fn after(&self) -> &'a NativeDocument {
        &self.record.after
    }
    pub fn token(&self) -> DocumentKey {
        self.record.identity
    }
}
impl TaskRequest {
    /// Allocate the bounded capture slots on the worker before taking the Engine
    /// mutex. Filling them later clones strong owners without allocation or I/O.
    pub fn new(
        current: &NativeDocument,
        actor: &str,
        owner: &str,
        task: &str,
        check: &dyn Fn() -> Result<()>,
    ) -> Result<Self> {
        if owner == "human" {
            return Err(
                "Selective undo is for agent tasks; human history remains chronological".into(),
            );
        }
        if actor != "human" && actor != owner {
            return Err("An agent can undo only its own tasks".into());
        }
        if actor.is_empty()
            || owner.is_empty()
            || [actor, owner, task].iter().any(|v| v.len() > 4096)
        {
            return Err("Invalid native task identity".into());
        }
        #[derive(Serialize)]
        struct Borrowed<'a> {
            actor: &'a str,
            owner: &'a str,
            task: &'a str,
        }
        let data = copy_admitted(
            &Borrowed { actor, owner, task },
            current.budget(),
            true,
            check,
        )?;
        let bytes = 200 * std::mem::size_of::<Arc<Record>>() + 128;
        let charge = current
            .budget()
            .reserve_bytes(bytes, bytes)
            .map_err(|e| e.to_string())?;
        let mut records = Vec::new();
        records
            .try_reserve_exact(200)
            .map_err(|_| "Native task capture allocation refused")?;
        if records.capacity() > 200 {
            return Err("Native task capture exceeds admission".into());
        }
        check()?;
        Ok(Self {
            base: current.key(),
            data,
            records,
            cancellation: Cancellation::default(),
            _charge: charge,
        })
    }
    pub fn cancellation(&self) -> Cancellation {
        self.cancellation.clone()
    }
}
impl NativeHistory {
    /// Call under the observed Engine mutex. No backing I/O, parameter clone or
    /// unbounded allocation occurs here. This captures data, not edit authority.
    pub fn capture_task(
        &self,
        current: &NativeDocument,
        mut request: TaskRequest,
    ) -> Result<TaskCapture> {
        request.cancellation.check()?;
        if request.base != current.key()
            || !request.data.owner().uses_budget(current.budget())
            || !self.pool.same_pool(current.budget())
            || self.current.is_some_and(|key| key != current.key())
        {
            return Err("Native task capture belongs to another history or generation".into());
        }
        if self
            .truncated
            .contains(&request.data.owner, &request.data.task)
        {
            return Err("Task exceeds retained history; refusing a partial task undo".into());
        }
        if self.undo.len() > request.records.capacity() {
            return Err("Native task history exceeds its capture admission".into());
        }
        request.records.extend(self.undo.iter().cloned());
        let captured = TaskCapture {
            current: current.clone(),
            request,
            history: self.identity,
            epoch: self.epoch,
        };
        if captured.entries().next().is_none() {
            return Err("No active batches for this task in retained history".into());
        }
        Ok(captured)
    }
}
impl TaskCapture {
    pub fn current(&self) -> &NativeDocument {
        &self.current
    }
    pub fn actor(&self) -> &str {
        &self.request.data.actor
    }
    pub fn owner(&self) -> &str {
        &self.request.data.owner
    }
    pub fn task(&self) -> &str {
        &self.request.data.task
    }
    pub fn cancellation(&self) -> Cancellation {
        self.request.cancellation.clone()
    }
    pub fn entries(&self) -> impl Iterator<Item = TaskEntry<'_>> + DoubleEndedIterator {
        self.request
            .records
            .iter()
            .rev()
            .filter(|record| {
                record.info.actor == self.request.data.owner
                    && record.info.task.as_deref() == Some(&self.request.data.task)
                    && !self
                        .request
                        .records
                        .iter()
                        .any(|later| later.info.reverted.contains(&record.identity.0))
            })
            .map(|record| TaskEntry { record })
    }
    pub fn check_current(&self, history: &NativeHistory, current: &NativeDocument) -> Result<()> {
        self.request.cancellation.check()?;
        if self.history != history.identity
            || self.epoch != history.epoch
            || self.current.key() != current.key()
        {
            return Err("Native task history cursor or document generation changed".into());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn commit(
        history: &mut NativeHistory,
        current: &mut NativeDocument,
        actor: &str,
        task: Option<&str>,
        visible: bool,
    ) {
        let layer = current.metadata().layer(0).unwrap().id().to_owned();
        let command = serde_json::json!({"op":"layer.update","layer":layer,"visible":visible});
        let descriptor = CommandDescriptor::from_normalized(
            current,
            current.metadata().revision(),
            &[command],
            &|| Ok(()),
        )
        .unwrap();
        let delta = current
            .capture_descriptor(descriptor, actor, &[], 1, false, true)
            .unwrap()
            .visibility(&layer, visible, &|| Ok(()))
            .unwrap();
        let prepared = history
            .snapshot()
            .prepare_commit(
                current,
                delta,
                HistoryRequest {
                    actor,
                    label: "Task capture",
                    task,
                    gesture: None,
                },
                &|| Ok(()),
            )
            .unwrap();
        drop(
            history
                .commit(
                    current,
                    Publication {
                        actor,
                        leases: &[],
                        now: 1,
                        closed: false,
                        write_available: true,
                    },
                    prepared,
                )
                .unwrap(),
        );
    }
    #[test]
    fn captures_strong_history_sources_in_reverse_task_order_and_rejects_stale_or_cancelled() {
        let pool = ResidentBudget::new(16 * 1024 * 1024).unwrap();
        let mut current = super::super::super::tests::document(&pool);
        let mut history = NativeHistory::new(&pool, 4).unwrap();
        commit(&mut history, &mut current, "agent", Some("task"), false);
        commit(&mut history, &mut current, "human", None, true);
        commit(&mut history, &mut current, "agent", Some("task"), false);
        let request = TaskRequest::new(&current, "human", "agent", "task", &|| Ok(())).unwrap();
        let capture = history.capture_task(&current, request).unwrap();
        assert_eq!(
            capture
                .entries()
                .map(|entry| entry.after().metadata().revision())
                .collect::<Vec<_>>(),
            vec![3, 1]
        );
        assert_eq!(capture.actor(), "human");
        assert_eq!(capture.owner(), "agent");
        assert_eq!(capture.task(), "task");
        capture.check_current(&history, &current).unwrap();
        commit(&mut history, &mut current, "human", None, true);
        assert!(capture.check_current(&history, &current).is_err());
        let request = TaskRequest::new(&current, "agent", "agent", "task", &|| Ok(())).unwrap();
        request.cancellation().cancel();
        assert!(history.capture_task(&current, request).is_err());
        assert!(TaskRequest::new(&current, "other", "agent", "task", &|| Ok(())).is_err());
        assert!(TaskRequest::new(&current, "human", "human", "task", &|| Ok(())).is_err());
        drop(history);
        drop(current);
        assert_eq!(
            capture
                .entries()
                .next()
                .unwrap()
                .before()
                .metadata()
                .revision(),
            2
        );
        drop(capture);
        assert_eq!(pool.used(), 0);
    }
    #[test]
    fn evicting_any_task_batch_refuses_partial_compensation_and_bound_history_cannot_cross_documents(
    ) {
        let pool = ResidentBudget::new(16 * 1024 * 1024).unwrap();
        let mut current = super::super::super::tests::document(&pool);
        let mut history = NativeHistory::new(&pool, 2).unwrap();
        commit(&mut history, &mut current, "agent", Some("task"), false);
        commit(&mut history, &mut current, "agent", Some("task"), true);
        commit(&mut history, &mut current, "human", None, false);
        let request = TaskRequest::new(&current, "human", "agent", "task", &|| Ok(())).unwrap();
        let refused = history.capture_task(&current, request).err().unwrap();
        assert!(refused.contains("partial task undo"));
        let other = super::super::super::tests::document(&pool);
        let request = TaskRequest::new(&other, "human", "agent", "task", &|| Ok(())).unwrap();
        assert!(history.capture_task(&other, request).is_err());
        let layer = other.metadata().layer(0).unwrap().id().to_owned();
        let delta = other
            .capture_intent(
                other.key(),
                0,
                "human",
                &[Scope::layer(&layer)],
                &[],
                1,
                false,
                true,
            )
            .unwrap()
            .visibility(&layer, false, &|| Ok(()))
            .unwrap();
        assert!(history
            .snapshot()
            .prepare_commit(
                &other,
                delta,
                HistoryRequest {
                    actor: "human",
                    label: "foreign",
                    task: None,
                    gesture: None
                },
                &|| Ok(())
            )
            .is_err());
        drop(history);
        drop(current);
        drop(other);
        assert_eq!(pool.used(), 0);
    }
    #[test]
    fn compensation_markers_exclude_already_reverted_records_and_chronological_undo_reactivates_them(
    ) {
        let pool = ResidentBudget::new(16 * 1024 * 1024).unwrap();
        let mut current = super::super::super::tests::document(&pool);
        let mut history = NativeHistory::new(&pool, 4).unwrap();
        commit(&mut history, &mut current, "agent", Some("task"), false);
        let reverted = history.undo[0].identity.0;
        commit(&mut history, &mut current, "human", None, true);
        let info = Info {
            actor: "human".into(),
            label: "Compensation marker".into(),
            task: None,
            gesture: None,
            scopes: vec![],
            reverted: vec![reverted],
        };
        let info = copy_admitted(&info, &pool, true, &|| Ok(())).unwrap();
        Arc::get_mut(history.undo.last_mut().unwrap()).unwrap().info = info;
        let request = TaskRequest::new(&current, "human", "agent", "task", &|| Ok(())).unwrap();
        assert!(history.capture_task(&current, request).is_err());
        let undo = history
            .snapshot()
            .prepare_move(&current, Direction::Undo, "human", &|| Ok(()))
            .unwrap();
        drop(
            history
                .move_cursor(
                    &mut current,
                    Publication {
                        actor: "human",
                        leases: &[],
                        now: 1,
                        closed: false,
                        write_available: true,
                    },
                    undo,
                )
                .unwrap(),
        );
        let request = TaskRequest::new(&current, "human", "agent", "task", &|| Ok(())).unwrap();
        let capture = history.capture_task(&current, request).unwrap();
        assert_eq!(capture.entries().count(), 1);
        drop(capture);
        drop(history);
        drop(current);
        assert_eq!(pool.used(), 0);
    }
}
