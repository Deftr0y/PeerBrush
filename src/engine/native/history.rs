//! Prepared history mutations: source work and allocation precede publication.
use super::*;
use crate::{
    engine::{Lease, Scope},
    large_document::Reservation,
};
use serde::Deserialize;

pub(crate) struct NativeHistory {
    identity: DocumentKey,
    epoch: u64,
    limit: usize,
    undo: Vec<Arc<Record>>,
    redo: Vec<Arc<Record>>,
    _capacity: Reservation,
}
pub(crate) struct HistorySnapshot {
    identity: DocumentKey,
    epoch: u64,
    limit: usize,
    undo: Option<Arc<Record>>,
    redo: Option<Arc<Record>>,
}
struct Record {
    identity: DocumentKey,
    before: NativeDocument,
    after: NativeDocument,
    info: Admitted<Info>,
    _charge: Reservation,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Info {
    actor: String,
    label: String,
    task: Option<String>,
    gesture: Option<String>,
    scopes: Vec<Scope>,
}
pub(crate) struct HistoryRequest<'a> {
    pub actor: &'a str,
    pub label: &'a str,
    pub task: Option<&'a str>,
    pub gesture: Option<&'a str>,
}
/// The Engine supplies these observations while holding its final write fence.
pub(crate) struct Publication<'a> {
    pub actor: &'a str,
    pub leases: &'a [Lease],
    pub now: u64,
    pub closed: bool,
    pub write_available: bool,
}
pub(crate) struct PreparedCommit {
    identity: DocumentKey,
    epoch: u64,
    delta: PreparedDelta,
    record: Arc<Record>,
    coalesce: bool,
    retired: Vec<Arc<Record>>,
    _retirement_charge: Reservation,
}
#[must_use]
pub(crate) struct RefusedCommit {
    reason: String,
    prepared: PreparedCommit,
}
impl std::fmt::Debug for RefusedCommit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("RefusedCommit").field(&self.reason).finish()
    }
}
impl RefusedCommit {
    pub fn reason(&self) -> &str {
        &self.reason
    }
    /// Drops history's additional candidate view before returning source tokens.
    pub fn into_prepared_delta(self) -> PreparedDelta {
        let PreparedCommit {
            delta,
            record,
            retired,
            ..
        } = self.prepared;
        drop(record);
        drop(retired);
        delta
    }
    pub fn abort(
        self,
        session: &impl super::super::scene::StageSession,
        cleanup: &dyn Fn() -> Result<()>,
    ) -> Result<()> {
        let Self { reason, prepared } = self;
        let PreparedCommit {
            delta,
            record,
            retired,
            ..
        } = prepared;
        drop(record);
        drop(retired);
        RefusedDelta {
            reason,
            prepared: delta,
        }
        .abort(session, cleanup)
    }
}
/// The caller releases this after leaving the Engine mutex. It can contain the
/// final owner of a retired metadata allocation or physical source Store.
#[must_use]
pub(crate) struct RetiredHistory {
    _previous: NativeDocument,
    _records: Vec<Arc<Record>>,
    _charge: Reservation,
}
impl std::fmt::Debug for RetiredHistory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RetiredHistory")
            .field("records", &self._records.len())
            .finish_non_exhaustive()
    }
}
#[derive(Clone, Copy)]
pub(crate) enum Direction {
    Undo,
    Redo,
}
pub(crate) struct PreparedMove {
    identity: DocumentKey,
    epoch: u64,
    record: DocumentKey,
    direction: Direction,
    actor: Admitted<String>,
    delta: PreparedDelta,
}
#[must_use]
pub(crate) struct RefusedMove {
    reason: String,
    prepared: PreparedMove,
}
impl std::fmt::Debug for RefusedMove {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("RefusedMove").field(&self.reason).finish()
    }
}
impl RefusedMove {
    pub fn reason(&self) -> &str {
        &self.reason
    }
    pub fn into_prepared(self) -> PreparedMove {
        self.prepared
    }
}
impl NativeHistory {
    pub fn new(pool: &ResidentBudget, limit: usize) -> Result<Self> {
        if !(1..=200).contains(&limit) {
            return Err("Native history limit must be between 1 and 200".into());
        }
        let bytes = limit
            .checked_mul(2 * std::mem::size_of::<Arc<Record>>())
            .and_then(|n| n.checked_add(128))
            .ok_or("Native history capacity overflow")?;
        let charge = pool
            .reserve_bytes(bytes, bytes)
            .map_err(|e| e.to_string())?;
        let mut undo = Vec::new();
        let mut redo = Vec::new();
        undo.try_reserve_exact(limit)
            .map_err(|_| "Native undo allocation refused")?;
        redo.try_reserve_exact(limit)
            .map_err(|_| "Native redo allocation refused")?;
        if undo.capacity() > limit || redo.capacity() > limit {
            return Err("Native history allocation exceeds its admission".into());
        }
        Ok(Self {
            identity: key(),
            epoch: 0,
            limit,
            undo,
            redo,
            _capacity: charge,
        })
    }
    /// Constant-size owner capture; no metadata copy or source I/O.
    pub fn snapshot(&self) -> HistorySnapshot {
        HistorySnapshot {
            identity: self.identity,
            epoch: self.epoch,
            limit: self.limit,
            undo: self.undo.last().cloned(),
            redo: self.redo.last().cloned(),
        }
    }
    pub fn undo_len(&self) -> usize {
        self.undo.len()
    }
    pub fn redo_len(&self) -> usize {
        self.redo.len()
    }
    /// Every fallible allocation and source operation has already completed.
    pub fn commit(
        &mut self,
        current: &mut NativeDocument,
        gate: Publication<'_>,
        prepared: PreparedCommit,
    ) -> std::result::Result<RetiredHistory, RefusedCommit> {
        let reason = if self.identity != prepared.identity || self.epoch != prepared.epoch {
            Some("Prepared native history cursor is stale")
        } else if self.epoch == u64::MAX {
            Some("Native history epoch overflow")
        } else if !gate.write_available {
            Some("Native document write is fenced")
        } else if prepared.record.info.actor != gate.actor {
            Some("Prepared native edit belongs to another actor")
        } else {
            None
        };
        if let Some(reason) = reason {
            return Err(RefusedCommit {
                reason: reason.into(),
                prepared,
            });
        }
        let PreparedCommit {
            identity,
            epoch,
            delta,
            record,
            coalesce,
            mut retired,
            _retirement_charge,
        } = prepared;
        let candidate = match current.publish(gate.actor, gate.leases, gate.now, gate.closed, delta)
        {
            Ok(candidate) => candidate,
            Err(refused) => {
                return Err(RefusedCommit {
                    reason: refused.reason,
                    prepared: PreparedCommit {
                        identity,
                        epoch,
                        delta: refused.prepared,
                        record,
                        coalesce,
                        retired,
                        _retirement_charge,
                    },
                })
            }
        };
        if coalesce {
            if let Some(old) = self.undo.pop() {
                retired.push(old);
            }
        } else if self.undo.len() == self.limit {
            retired.push(self.undo.remove(0));
        }
        self.undo.push(record);
        for old in self.redo.drain(..) {
            retired.push(old);
        }
        self.epoch += 1;
        let previous = std::mem::replace(current, candidate);
        Ok(RetiredHistory {
            _previous: previous,
            _records: retired,
            _charge: _retirement_charge,
        })
    }
    pub fn move_cursor(
        &mut self,
        current: &mut NativeDocument,
        gate: Publication<'_>,
        prepared: PreparedMove,
    ) -> std::result::Result<NativeDocument, RefusedMove> {
        let selected = match prepared.direction {
            Direction::Undo => self.undo.last(),
            Direction::Redo => self.redo.last(),
        };
        let reason = if self.identity != prepared.identity
            || self.epoch != prepared.epoch
            || selected.is_none_or(|record| record.identity != prepared.record)
        {
            Some("Prepared native history cursor is stale")
        } else if self.epoch == u64::MAX {
            Some("Native history epoch overflow")
        } else if !gate.write_available {
            Some("Native document write is fenced")
        } else if prepared.actor.as_str() != gate.actor {
            Some("Prepared native history move belongs to another actor")
        } else {
            None
        };
        if let Some(reason) = reason {
            return Err(RefusedMove {
                reason: reason.into(),
                prepared,
            });
        }
        let PreparedMove {
            identity,
            epoch,
            record,
            direction,
            actor,
            delta,
        } = prepared;
        let candidate = match current.publish(gate.actor, gate.leases, gate.now, gate.closed, delta)
        {
            Ok(candidate) => candidate,
            Err(refused) => {
                return Err(RefusedMove {
                    reason: refused.reason,
                    prepared: PreparedMove {
                        identity,
                        epoch,
                        record,
                        direction,
                        actor,
                        delta: refused.prepared,
                    },
                })
            }
        };
        match direction {
            Direction::Undo => {
                if let Some(record) = self.undo.pop() {
                    self.redo.push(record);
                }
            }
            Direction::Redo => {
                if let Some(record) = self.redo.pop() {
                    self.undo.push(record);
                }
            }
        }
        self.epoch += 1;
        Ok(std::mem::replace(current, candidate))
    }
}
impl HistorySnapshot {
    /// Run on the worker, retaining the prepared source tokens on every failure.
    pub fn prepare_commit(
        &self,
        current: &NativeDocument,
        delta: PreparedDelta,
        request: HistoryRequest<'_>,
        check: &dyn Fn() -> Result<()>,
    ) -> std::result::Result<PreparedCommit, RefusedDelta> {
        let build = || -> Result<(Arc<Record>, bool, Vec<Arc<Record>>, Reservation)> {
            delta.cancellation.check()?;
            check()?;
            if delta.base != current.key() {
                return Err("Prepared native edit is stale".into());
            }
            delta
                .admission
                .as_ref()
                .ok_or("Native editor capture admission is missing")?
                .check(current, request.actor, &delta.intent)?;
            let coalesce = request.gesture.is_some()
                && self.undo.as_ref().is_some_and(|record| {
                    record.info.actor == request.actor
                        && record.info.task.as_deref() == request.task
                        && record.info.gesture.as_deref() == request.gesture
                        && record.after.key() == current.key()
                });
            let old = self.undo.as_ref().filter(|_| coalesce);
            let count = old
                .map_or(0, |record| record.info.scopes.len())
                .checked_add(delta.intent.scopes().len())
                .ok_or("Native history scope overflow")?;
            let temporary_bytes = count
                .checked_mul(std::mem::size_of::<&Scope>())
                .and_then(|n| n.checked_add(128))
                .ok_or("Native history scope footprint overflow")?;
            let temporary = current
                .budget()
                .reserve_bytes(temporary_bytes, temporary_bytes)
                .map_err(|e| e.to_string())?;
            let mut scopes = Vec::new();
            scopes
                .try_reserve_exact(count)
                .map_err(|_| "Native history scope allocation refused")?;
            if let Some(old) = old {
                scopes.extend(old.info.scopes.iter());
            }
            scopes.extend(delta.intent.scopes().iter());
            #[derive(Serialize)]
            struct Borrowed<'a> {
                actor: &'a str,
                label: &'a str,
                task: Option<&'a str>,
                gesture: Option<&'a str>,
                scopes: Vec<&'a Scope>,
            }
            let borrowed = Borrowed {
                actor: request.actor,
                label: old.map_or(request.label, |record| record.info.label.as_str()),
                task: request.task,
                gesture: request.gesture,
                scopes,
            };
            let info: Admitted<Info> = copy_admitted(&borrowed, current.budget(), true, check)?;
            drop(borrowed);
            drop(temporary);
            let bytes = std::mem::size_of::<Record>() + 128;
            let charge = current
                .budget()
                .reserve_bytes(bytes, bytes)
                .map_err(|e| e.to_string())?;
            let record = Arc::new(Record {
                identity: key(),
                before: old.map_or_else(|| current.clone(), |record| record.before.clone()),
                after: delta.candidate.clone(),
                info,
                _charge: charge,
            });
            let bytes = (self.limit + 1)
                .checked_mul(std::mem::size_of::<Arc<Record>>())
                .and_then(|n| n.checked_add(128))
                .ok_or("Native retirement footprint overflow")?;
            let retirement_charge = current
                .budget()
                .reserve_bytes(bytes, bytes)
                .map_err(|e| e.to_string())?;
            let mut retired = Vec::new();
            retired
                .try_reserve_exact(self.limit + 1)
                .map_err(|_| "Native history retirement allocation refused")?;
            if retired.capacity() > self.limit + 1 {
                return Err("Native history retirement exceeds its admission".into());
            }
            delta.cancellation.check()?;
            check()?;
            Ok((record, coalesce, retired, retirement_charge))
        };
        match build() {
            Ok((record, coalesce, retired, charge)) => Ok(PreparedCommit {
                identity: self.identity,
                epoch: self.epoch,
                delta,
                record,
                coalesce,
                retired,
                _retirement_charge: charge,
            }),
            Err(reason) => Err(RefusedDelta {
                reason,
                prepared: delta,
            }),
        }
    }
    pub fn prepare_move(
        &self,
        current: &NativeDocument,
        direction: Direction,
        actor: &str,
        check: &dyn Fn() -> Result<()>,
    ) -> Result<PreparedMove> {
        check()?;
        let record = match direction {
            Direction::Undo => self.undo.as_ref(),
            Direction::Redo => self.redo.as_ref(),
        }
        .ok_or("Nothing to undo or redo")?;
        if actor != "human" && record.info.actor != actor {
            return Err("History belongs to another participant".into());
        }
        let source = match direction {
            Direction::Undo => &record.before,
            Direction::Redo => &record.after,
        };
        if source.metadata().id() != current.metadata().id() || current.metadata().read_only() {
            return Err("History belongs to another or read-only document".into());
        }
        let intent =
            Intent::from_scopes(IntentKind::History, &record.info.scopes, current.budget())?;
        let copied = copy_metadata(&source.core.metadata, current.budget(), check)?;
        let roster = source.core.roster.clone();
        let resident = current.budget().clone();
        let disk = current.disk_budget().clone();
        if !resident.same_pool(source.budget()) || !disk.same_pool(source.disk_budget()) {
            return Err("History source admission differs".into());
        }
        let core = copied.map_unique(|mut metadata, _| {
            metadata.revision = current
                .metadata()
                .revision()
                .checked_add(1)
                .ok_or("Native revision overflow")?;
            Ok(Core {
                metadata,
                roster,
                resident,
                disk,
                key: key(),
                render_key: OnceLock::new(),
            })
        })?;
        let actor: Admitted<String> = copy_admitted(&actor, current.budget(), true, check)?;
        check()?;
        Ok(PreparedMove {
            identity: self.identity,
            epoch: self.epoch,
            record: record.identity,
            direction,
            actor,
            delta: PreparedDelta {
                base: current.key(),
                candidate: NativeDocument { core },
                cancellation: Cancellation::default(),
                intent,
                outputs: vec![],
                admission: None,
            },
        })
    }
}

#[cfg(test)]
mod tests {
    use super::super::output::tests::{draft, Session};
    use super::*;
    fn gate(actor: &str) -> Publication<'_> {
        Publication {
            actor,
            leases: &[],
            now: 1,
            closed: false,
            write_available: true,
        }
    }
    fn request(actor: &str) -> HistoryRequest<'_> {
        HistoryRequest {
            actor,
            label: "edit",
            task: None,
            gesture: None,
        }
    }
    fn captured(doc: &NativeDocument) -> CapturedEdit {
        doc.capture_intent(
            doc.key(),
            doc.metadata().revision(),
            "worker",
            &[Scope {
                target: None,
                rect: None,
            }],
            &[],
            1,
            false,
            true,
        )
        .unwrap()
    }
    fn visibility(doc: &NativeDocument, visible: bool) -> PreparedDelta {
        captured(doc)
            .visibility(doc.metadata().layer(0).unwrap().id(), visible, &|| Ok(()))
            .unwrap()
    }
    #[test]
    fn undo_redo_restore_owned_headers_with_monotonic_revisions_without_source_io() {
        let pool = ResidentBudget::new(8 * 1024 * 1024).unwrap();
        let mut current = super::super::tests::document(&pool);
        let original_key = current
            .source(current.metadata().layer(0).unwrap().pixels().slot())
            .unwrap()
            .key();
        let mut history = NativeHistory::new(&pool, 4).unwrap();
        let delta = visibility(&current, false);
        let prepared = history
            .snapshot()
            .prepare_commit(&current, delta, request("worker"), &|| Ok(()))
            .unwrap_or_else(|e| panic!("{}", e.reason()));
        let retired = history
            .commit(&mut current, gate("worker"), prepared)
            .unwrap();
        drop(retired);
        assert_eq!((history.undo_len(), history.redo_len()), (1, 0));
        assert_eq!(current.metadata().revision(), 1);
        assert!(!current.metadata().layer(0).unwrap().visible());
        assert!(history
            .snapshot()
            .prepare_move(&current, Direction::Undo, "another", &|| Ok(()))
            .is_err());
        let undo = history
            .snapshot()
            .prepare_move(&current, Direction::Undo, "human", &|| Ok(()))
            .unwrap();
        drop(
            history
                .move_cursor(&mut current, gate("human"), undo)
                .unwrap(),
        );
        assert_eq!(current.metadata().revision(), 2);
        assert!(current.metadata().layer(0).unwrap().visible());
        let redo = history
            .snapshot()
            .prepare_move(&current, Direction::Redo, "worker", &|| Ok(()))
            .unwrap();
        drop(
            history
                .move_cursor(&mut current, gate("worker"), redo)
                .unwrap(),
        );
        assert_eq!(current.metadata().revision(), 3);
        assert!(!current.metadata().layer(0).unwrap().visible());
        assert_eq!(
            current
                .source(current.metadata().layer(0).unwrap().pixels().slot())
                .unwrap()
                .key(),
            original_key
        );
        assert_eq!(current.disk_budget().used().unwrap(), 0);
        drop(history);
        drop(current);
        assert_eq!(pool.used(), 0);
    }
    #[test]
    fn gesture_records_coalesce_without_inheriting_header_tickets_and_keep_original_label() {
        let pool = ResidentBudget::new(8 * 1024 * 1024).unwrap();
        let mut current = super::super::tests::document(&pool);
        let mut history = NativeHistory::new(&pool, 4).unwrap();
        for (visible, label) in [(false, "start"), (true, "finish")] {
            let delta = visibility(&current, visible);
            let prepared = history
                .snapshot()
                .prepare_commit(
                    &current,
                    delta,
                    HistoryRequest {
                        actor: "worker",
                        label,
                        task: Some("task"),
                        gesture: Some("gesture"),
                    },
                    &|| Ok(()),
                )
                .unwrap_or_else(|e| panic!("{}", e.reason()));
            drop(
                history
                    .commit(&mut current, gate("worker"), prepared)
                    .unwrap(),
            );
        }
        assert_eq!(history.undo_len(), 1);
        assert_eq!(history.undo.last().unwrap().info.label, "start");
        assert_eq!(history.undo.last().unwrap().before.metadata().revision(), 0);
        assert_eq!(history.undo.last().unwrap().after.metadata().revision(), 2);
        let undo = history
            .snapshot()
            .prepare_move(&current, Direction::Undo, "human", &|| Ok(()))
            .unwrap();
        drop(
            history
                .move_cursor(&mut current, gate("human"), undo)
                .unwrap(),
        );
        assert_eq!(current.metadata().revision(), 3);
        assert!(current.metadata().layer(0).unwrap().visible());
        drop(history);
        drop(current);
        assert_eq!(pool.used(), 0);
    }
    #[test]
    fn stale_history_cursor_and_final_fences_do_not_change_document_or_history() {
        let pool = ResidentBudget::new(8 * 1024 * 1024).unwrap();
        let mut current = super::super::tests::document(&pool);
        let mut history = NativeHistory::new(&pool, 2).unwrap();
        let snapshot = history.snapshot();
        let first = snapshot
            .prepare_commit(
                &current,
                visibility(&current, false),
                request("worker"),
                &|| Ok(()),
            )
            .unwrap_or_else(|e| panic!("{}", e.reason()));
        let second = snapshot
            .prepare_commit(
                &current,
                visibility(&current, true),
                request("worker"),
                &|| Ok(()),
            )
            .unwrap_or_else(|e| panic!("{}", e.reason()));
        let mut fenced = gate("worker");
        fenced.write_available = false;
        let refused = history.commit(&mut current, fenced, first).unwrap_err();
        assert!(refused.reason().contains("fenced"));
        assert_eq!(current.metadata().revision(), 0);
        assert_eq!(history.undo_len(), 0);
        let first = refused.prepared;
        drop(history.commit(&mut current, gate("worker"), first).unwrap());
        let refused = history
            .commit(&mut current, gate("worker"), second)
            .unwrap_err();
        assert!(refused.reason().contains("cursor"));
        drop(refused);
        assert_eq!(current.metadata().revision(), 1);
        assert_eq!(history.undo_len(), 1);
        let undo = history
            .snapshot()
            .prepare_move(&current, Direction::Undo, "human", &|| Ok(()))
            .unwrap();
        let scopes = history.undo.last().unwrap().info.scopes.clone();
        let lease = Lease {
            id: "late".into(),
            owner: "other".into(),
            description: "history".into(),
            scopes,
            expires: 100,
        };
        let refused = history
            .move_cursor(
                &mut current,
                Publication {
                    leases: &[lease],
                    ..gate("human")
                },
                undo,
            )
            .unwrap_err();
        assert!(refused.reason().contains("Reserved"));
        assert_eq!(current.metadata().revision(), 1);
        assert_eq!((history.undo_len(), history.redo_len()), (1, 0));
        drop(refused);
    }
    #[test]
    fn native_pixel_history_retains_exact_sources_and_refused_commit_keeps_retirement_tokens() {
        let pool = ResidentBudget::new(16 * 1024 * 1024).unwrap();
        let mut current = super::super::tests::document(&pool);
        let mut history = NativeHistory::new(&pool, 2).unwrap();
        let (built, path) = draft(&current);
        let output = built
            .seal(&current, &Session, &|| Ok(()))
            .unwrap_or_else(|e| panic!("{}", e.reason()));
        let target = current.metadata().layer(0).unwrap().pixels().slot();
        let delta = captured(&current)
            .raster_patch(
                RasterPatch {
                    target,
                    output,
                    retained: RetainedUpdate::Keep,
                    layer_origin: None,
                },
                &|| Ok(()),
            )
            .unwrap_or_else(|e| panic!("{}", e.reason()));
        let prepared = history
            .snapshot()
            .prepare_commit(&current, delta, request("worker"), &|| Ok(()))
            .unwrap_or_else(|e| panic!("{}", e.reason()));
        let wrong = history
            .commit(&mut current, gate("other"), prepared)
            .unwrap_err();
        assert!(wrong.reason().contains("actor"));
        wrong.abort(&Session, &|| Ok(())).unwrap();
        assert_eq!(current.disk_budget().used().unwrap(), 0);
        std::fs::remove_dir(path).unwrap();
        let (built, path) = draft(&current);
        let output = built
            .seal(&current, &Session, &|| Ok(()))
            .unwrap_or_else(|e| panic!("{}", e.reason()));
        let delta = captured(&current)
            .raster_patch(
                RasterPatch {
                    target,
                    output,
                    retained: RetainedUpdate::Keep,
                    layer_origin: None,
                },
                &|| Ok(()),
            )
            .unwrap_or_else(|e| panic!("{}", e.reason()));
        let prepared = history
            .snapshot()
            .prepare_commit(&current, delta, request("worker"), &|| Ok(()))
            .unwrap_or_else(|e| panic!("{}", e.reason()));
        drop(
            history
                .commit(&mut current, gate("worker"), prepared)
                .unwrap(),
        );
        let bytes = current.disk_budget().used().unwrap();
        let physical = current
            .source(current.metadata().layer(0).unwrap().pixels().slot())
            .unwrap()
            .key();
        let undo = history
            .snapshot()
            .prepare_move(&current, Direction::Undo, "human", &|| Ok(()))
            .unwrap();
        drop(
            history
                .move_cursor(&mut current, gate("human"), undo)
                .unwrap(),
        );
        assert!(current
            .source(current.metadata().layer(0).unwrap().pixels().slot())
            .unwrap()
            .is_known_zero());
        assert_eq!(current.disk_budget().used().unwrap(), bytes);
        let redo = history
            .snapshot()
            .prepare_move(&current, Direction::Redo, "human", &|| Ok(()))
            .unwrap();
        drop(
            history
                .move_cursor(&mut current, gate("human"), redo)
                .unwrap(),
        );
        let source = current
            .source(current.metadata().layer(0).unwrap().pixels().slot())
            .unwrap();
        assert_eq!(source.key(), physical);
        let region = source
            .prepare_region(
                crate::raster::backed::Region::new([0, 0, 1, 1]).unwrap(),
                &mut || Ok(()),
            )
            .unwrap();
        assert_eq!(region.pixel16(0, 0).unwrap(), [12345, 23456, 34567, 65535]);
        drop(region);
        assert_eq!(current.disk_budget().used().unwrap(), bytes);
        drop(history);
        drop(current);
        std::fs::remove_dir_all(path).unwrap();
        assert_eq!(pool.used(), 0);
    }
}
