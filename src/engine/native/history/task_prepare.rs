//! Selective compensation is prepared outside the Engine mutex and adopts once.
use super::{metadata_inverse, task_sources::Sources, *};
use crate::engine::scene::StageSession;

pub(crate) struct TaskCandidate {
    capture: TaskCapture,
    candidate: NativeDocument,
    outputs: Vec<StagedOutput>,
    scopes: Intent,
    reverted: Admitted<Vec<[u8; 16]>>,
    // Holds the actual output-vector allocation until SourcePublication takes it.
    _charge: Option<Arc<Reservation>>,
}
pub(crate) struct TaskPreparationError {
    pub reason: String,
    pub report: Option<Admitted<metadata_inverse::Report>>,
    pub cleanup_error: Option<String>,
}
#[must_use]
pub(crate) struct RefusedTask {
    reason: String,
    candidate: TaskCandidate,
}
struct CommitParts {
    record: Arc<Record>,
    retired: Vec<Arc<Record>>,
    charge: Reservation,
    sources: Option<super::super::publication::SourcePublication>,
    truncated: Arc<super::task_capture::TruncatedTasks>,
    admission: Arc<super::super::admission::EditAdmission>,
}
impl std::fmt::Debug for RefusedTask {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("RefusedTask").field(&self.reason).finish()
    }
}
impl RefusedTask {
    pub fn reason(&self) -> &str {
        &self.reason
    }
    pub fn abort(
        self,
        session: &impl StageSession,
        cleanup: &dyn Fn() -> Result<()>,
    ) -> Result<()> {
        self.candidate.abort(session, cleanup)
    }
}
impl TaskCandidate {
    pub fn preview(&self) -> &NativeDocument {
        &self.candidate
    }
    pub fn abort(
        self,
        session: &impl StageSession,
        cleanup: &dyn Fn() -> Result<()>,
    ) -> Result<()> {
        let Self {
            capture,
            candidate,
            outputs,
            scopes,
            reverted,
            _charge,
        } = self;
        drop((capture, candidate, scopes, reverted));
        let result = super::super::output::abort_outputs(outputs, session, cleanup);
        drop(_charge);
        result
    }
}
impl TaskCapture {
    /// Every source read, COW tile, seal, metadata merge and cancellation check
    /// occurs on the worker. A report conflict refuses the WHOLE task.
    pub fn prepare(
        self,
        session: &impl StageSession,
        check: &dyn Fn() -> Result<()>,
    ) -> std::result::Result<TaskCandidate, TaskPreparationError> {
        let cancellation = self.cancellation();
        let checked = || {
            cancellation.check()?;
            session.check()?;
            check()?;
            cancellation.check()
        };
        let mut working = self.current().clone();
        let mut outputs = Vec::new();
        let mut output_charge = None;
        let mut conflict_report = None;
        let mut build = || -> Result<(NativeDocument, Intent, Admitted<Vec<[u8; 16]>>)> {
            checked()?;
            if working.metadata().read_only() {
                return Err("Native task document is read-only".into());
            }
            if self.entries().next().is_none() {
                return Err("No active batches for this task in retained history".into());
            }
            let mut scopes = Intent::from_scopes(IntentKind::History, &[], working.budget())?;
            for entry in self.entries() {
                checked()?;
                let mut sources =
                    Sources::new(entry.before(), entry.after(), &working, session, &checked)?;
                let mut step = || -> Result<(NativeDocument, Intent)> {
                    let current =
                        copy_metadata(&working.core.metadata, working.budget(), &checked)?;
                    let result = metadata_inverse::inverse(
                        &entry.before().core.metadata,
                        &entry.after().core.metadata,
                        current,
                        &mut sources,
                        working.budget(),
                        &checked,
                    )?;
                    if !result.report.conflicts.is_empty() {
                        conflict_report = Some(result.report);
                        return Err(
                            "Task conflicts with later work; no compensation was applied".into(),
                        );
                    }
                    let step_scopes = Intent::from_scopes(
                        IntentKind::History,
                        &result.report.scopes,
                        working.budget(),
                    )?;
                    let mut next_scopes = scopes.combined(&step_scopes, working.budget())?;
                    // Visibility reservations are a separate domain from broad
                    // layer/pixel reservations. A metadata compensation must
                    // recheck that domain too when it actually changes it.
                    for layer in &result.candidate.layers {
                        if working
                            .core
                            .metadata
                            .layers
                            .iter()
                            .any(|old| old.id == layer.id && old.visible != layer.visible)
                        {
                            let visibility =
                                Intent::visibility(&layer.id, layer.visible, working.budget())?;
                            next_scopes = next_scopes.combined(&visibility, working.budget())?;
                        }
                    }
                    let count = outputs
                        .len()
                        .checked_add(sources.output_count())
                        .ok_or("Native task output count overflow")?;
                    let bytes = count
                        .checked_mul(std::mem::size_of::<StagedOutput>())
                        .and_then(|n| n.checked_add(128))
                        .ok_or("Native task output footprint overflow")?;
                    let charge = working
                        .budget()
                        .reserve_bytes(bytes, bytes)
                        .map_err(|e| e.to_string())?;
                    outputs
                        .try_reserve_exact(sources.output_count())
                        .map_err(|_| "Native task output allocation refused")?;
                    if outputs.capacity() > count {
                        return Err("Native task output vector exceeds admission".into());
                    }
                    // The vector can already have grown if a subsequent source
                    // or document check fails. Retain its new charge on BOTH paths.
                    output_charge = Some(Arc::new(charge));
                    let document = sources.document(result.candidate)?;
                    checked()?;
                    Ok((document, next_scopes))
                };
                match step() {
                    Ok((document, next_scopes)) => {
                        outputs.extend(sources.take_outputs());
                        // Source workspace admission remains held through append.
                        drop(sources);
                        working = document;
                        scopes = next_scopes;
                    }
                    Err(reason) => {
                        let cleanup = sources.abort();
                        return Err(match cleanup {
                            Ok(()) => reason,
                            Err(cleanup) => format!(
                                "{reason}; provisional source retirement refused: {cleanup}"
                            ),
                        });
                    }
                }
            }
            struct Reverted<'a>(&'a TaskCapture);
            impl Serialize for Reverted<'_> {
                fn serialize<S: Serializer>(
                    &self,
                    serializer: S,
                ) -> std::result::Result<S::Ok, S::Error> {
                    use serde::ser::SerializeSeq;
                    let mut sequence = serializer.serialize_seq(Some(self.0.entries().count()))?;
                    for entry in self.0.entries() {
                        sequence.serialize_element(&entry.token().0)?;
                    }
                    sequence.end()
                }
            }
            let reverted = copy_admitted(&Reverted(&self), working.budget(), true, &checked)?;
            let candidate = super::super::transaction::restamp(
                &working,
                self.current()
                    .metadata()
                    .revision()
                    .checked_add(1)
                    .ok_or("Native revision overflow")?,
                &checked,
            )?;
            checked()?;
            Ok((candidate, scopes, reverted))
        };
        match build() {
            Ok((candidate, scopes, reverted)) => Ok(TaskCandidate {
                capture: self,
                candidate,
                outputs,
                scopes,
                reverted,
                _charge: output_charge,
            }),
            Err(reason) => {
                drop(working);
                let cleanup_error = super::super::output::abort_outputs(outputs, session, &|| {
                    session.cleanup_check()
                })
                .err();
                drop(output_charge);
                Err(TaskPreparationError {
                    reason,
                    report: conflict_report,
                    cleanup_error,
                })
            }
        }
    }
}
impl HistorySnapshot {
    /// Returns the normal final-guarded history packet. There is no raw metadata
    /// setter or arbitrary source roster input on this nominal task path.
    pub fn prepare_task(
        &self,
        candidate: TaskCandidate,
        check: &dyn Fn() -> Result<()>,
    ) -> std::result::Result<PreparedCommit, RefusedTask> {
        let build = || -> Result<CommitParts> {
            candidate.capture.cancellation().check()?;
            check()?;
            let current = candidate.capture.current();
            if self.identity != candidate.capture.history
                || self.epoch != candidate.capture.epoch
                || self.current.is_some_and(|key| key != current.key())
                || !self.pool.same_pool(current.budget())
                || candidate.candidate.metadata().id() != current.metadata().id()
                || current.metadata().revision().checked_add(1)
                    != Some(candidate.candidate.metadata().revision())
            {
                return Err("Native task capture or history cursor changed".into());
            }
            #[derive(Serialize)]
            struct Borrowed<'a> {
                actor: &'a str,
                label: &'a str,
                task: Option<&'a str>,
                gesture: Option<&'a str>,
                scopes: &'a [Scope],
                reverted: &'a [[u8; 16]],
                reverted_revisions: Revisions<'a>,
            }
            struct Revisions<'a>(&'a TaskCapture);
            impl Serialize for Revisions<'_> {
                fn serialize<S: Serializer>(
                    &self,
                    serializer: S,
                ) -> std::result::Result<S::Ok, S::Error> {
                    use serde::ser::SerializeSeq;
                    let mut sequence = serializer.serialize_seq(Some(self.0.entries().count()))?;
                    for entry in self.0.entries() {
                        sequence.serialize_element(&entry.after().metadata().revision())?;
                    }
                    sequence.end()
                }
            }
            let info = copy_admitted(
                &Borrowed {
                    actor: candidate.capture.actor(),
                    label: "Undo task",
                    task: None,
                    gesture: None,
                    scopes: candidate.scopes.scopes(),
                    reverted: &candidate.reverted,
                    reverted_revisions: Revisions(&candidate.capture),
                },
                current.budget(),
                true,
                check,
            )?;
            let bytes = std::mem::size_of::<Record>() + 128;
            let charge = current
                .budget()
                .reserve_bytes(bytes, bytes)
                .map_err(|e| e.to_string())?;
            let record = Arc::new(Record {
                identity: key(),
                before: current.clone(),
                after: candidate.candidate.clone(),
                info,
                _charge: charge,
            });
            let bytes = (self.limit + 1)
                .checked_mul(std::mem::size_of::<Arc<Record>>())
                .and_then(|n| n.checked_add(128))
                .ok_or("Native task retirement footprint overflow")?;
            let charge = current
                .budget()
                .reserve_bytes(bytes, bytes)
                .map_err(|e| e.to_string())?;
            let mut retired = Vec::new();
            retired
                .try_reserve_exact(self.limit + 1)
                .map_err(|_| "Native task retirement allocation refused")?;
            if retired.capacity() > self.limit + 1 {
                return Err("Native task retirement exceeds admission".into());
            }
            let sources = if candidate.outputs.is_empty() {
                None
            } else {
                Some(super::super::publication::SourcePublication::prepare(
                    &candidate.candidate,
                    &candidate.outputs,
                    check,
                )?)
            };
            let truncated = if self.undo_len == self.limit {
                self.truncated
                    .retaining_evicted(self.oldest.as_deref(), current.budget(), check)?
            } else {
                self.truncated.clone()
            };
            let admission = super::super::admission::EditAdmission::task(
                &candidate.capture,
                &candidate.scopes,
                candidate._charge.clone(),
                check,
            )?;
            candidate.capture.cancellation().check()?;
            check()?;
            Ok(CommitParts {
                record,
                retired,
                charge,
                sources,
                truncated,
                admission,
            })
        };
        match build() {
            Ok(CommitParts {
                record,
                retired,
                charge,
                sources,
                truncated,
                admission,
            }) => {
                let TaskCandidate {
                    capture,
                    candidate,
                    outputs,
                    scopes,
                    reverted,
                    _charge,
                } = candidate;
                let delta = PreparedDelta {
                    base: capture.current().key(),
                    candidate,
                    cancellation: capture.cancellation(),
                    intent: scopes,
                    outputs,
                    admission: Some(admission),
                };
                // Both refusal-returned Delta and the final publication plan
                // retain admission for the actual output vector lifetime.
                drop((_charge, reverted, capture));
                Ok(PreparedCommit {
                    identity: self.identity,
                    epoch: self.epoch,
                    delta,
                    record,
                    coalesce: false,
                    retired,
                    sources,
                    truncated,
                    _retirement_charge: charge,
                })
            }
            Err(reason) => Err(RefusedTask { reason, candidate }),
        }
    }
}
#[cfg(test)]
#[path = "task_prepare_tests.rs"]
mod tests;
