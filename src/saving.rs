//! Working-save progress and its one-way cancellation/publication boundary.
use std::sync::{Arc, Mutex};

#[derive(Clone)]
pub struct Control(Arc<Mutex<State>>);
/// Prepared outside the final Engine fence; successful consumption allocates
/// nothing and retains the exact begun request identity.
pub(crate) struct PreparedCommit {
    control: Control,
    document: String,
    run: String,
    revision: u64,
    stage: String,
}
pub(crate) struct CommittedControl {
    control: Control,
    document: String,
    run: String,
    revision: u64,
}
struct State {
    started: bool,
    status: Status,
}
#[derive(Clone, serde::Serialize)]
pub struct Status {
    pub run: String,
    pub document_id: String,
    pub revision: u64,
    pub stage: String,
    pub completed: usize,
    pub total: usize,
    pub can_cancel: bool,
    pub cancel_requested: bool,
    pub finished: bool,
    pub error: Option<String>,
}
impl Default for Control {
    fn default() -> Self {
        Self(Arc::new(Mutex::new(State {
            started: false,
            status: Status {
                run: crate::engine::id(),
                document_id: String::new(),
                revision: 0,
                stage: "Preparing save".into(),
                completed: 0,
                total: 0,
                can_cancel: true,
                cancel_requested: false,
                finished: false,
                error: None,
            },
        })))
    }
}
impl Control {
    pub(crate) fn prepare_commit(
        &self,
        document: &str,
        revision: u64,
    ) -> Result<PreparedCommit, String> {
        let state = self.0.lock().map_err(|_| "Save state is unavailable")?;
        if document.is_empty()
            || !state.started
            || state.status.document_id != document
            || state.status.revision != revision
            || state.status.finished
            || !state.status.can_cancel
            || state.status.cancel_requested
        {
            return Err("Save request cannot arm this source publication".into());
        }
        fn copy(value: &str) -> Result<String, String> {
            let mut result = String::new();
            result
                .try_reserve_exact(value.len())
                .map_err(|_| "Save fence allocation refused")?;
            result.push_str(value);
            Ok(result)
        }
        Ok(PreparedCommit {
            control: self.clone(),
            document: copy(document)?,
            run: copy(&state.status.run)?,
            revision,
            stage: copy("Finishing save")?,
        })
    }
    pub(crate) fn begin(&self, document: String, revision: u64) -> Result<(), String> {
        let mut state = self.0.lock().unwrap();
        if state.started {
            return Err("This save request has already started".into());
        }
        state.started = true;
        state.status.document_id = document;
        state.status.revision = revision;
        Self::check_status(&state.status)
    }
    pub(crate) fn same(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
    pub fn status(&self) -> Status {
        self.0.lock().unwrap().status.clone()
    }
    fn check_status(status: &Status) -> Result<(), String> {
        if status.cancel_requested {
            Err("Save canceled; current work and destination preserved".into())
        } else {
            Ok(())
        }
    }
    pub fn check(&self) -> Result<(), String> {
        Self::check_status(&self.0.lock().unwrap().status)
    }
    /// Publication may verify a large file without holding the engine mutex.
    /// Reads/navigation remain available; mutations wait for the final outcome.
    pub(crate) fn ensure_editable(&self) -> Result<(), String> {
        let state = self.0.lock().unwrap();
        if state.started
            && !state.status.finished
            && !state.status.can_cancel
            && !state.status.cancel_requested
        {
            Err("Finishing save publication; wait before changing this project".into())
        } else {
            Ok(())
        }
    }
    /// True only when this request accepted cancellation before publication.
    pub fn cancel(&self) -> bool {
        let mut state = self.0.lock().unwrap();
        if !state.status.can_cancel || state.status.finished {
            return false;
        }
        state.status.cancel_requested = true;
        state.status.can_cancel = false;
        state.status.stage = "Canceling save".into();
        true
    }
    pub(crate) fn progress(
        &self,
        stage: &str,
        completed: usize,
        total: usize,
    ) -> Result<(), String> {
        let mut state = self.0.lock().unwrap();
        Self::check_status(&state.status)?;
        if state.status.stage != stage {
            state.status.stage = stage.into();
        }
        state.status.completed = completed;
        state.status.total = total;
        Ok(())
    }
    pub(crate) fn commit(&self) -> Result<(), String> {
        let mut state = self.0.lock().unwrap();
        Self::check_status(&state.status)?;
        state.status.can_cancel = false;
        state.status.stage = "Finishing save".into();
        state.status.completed = 0;
        state.status.total = 0;
        Ok(())
    }
    pub(crate) fn finish(&self, result: &Result<(), String>) {
        let mut state = self.0.lock().unwrap();
        state.status.finished = true;
        state.status.can_cancel = false;
        state.status.error = result.as_ref().err().cloned();
        state.status.stage = if result.is_ok() {
            "Saved"
        } else if state.status.cancel_requested {
            "Save canceled"
        } else {
            "Save failed"
        }
        .into();
    }
}

impl PreparedCommit {
    /// The caller serializes its source-operation closing gate around this.
    /// All success-path strings and owners were allocated during preparation.
    pub(crate) fn commit(self, control: &Control) -> Result<CommittedControl, &'static str> {
        if !self.control.same(control) {
            return Err("Publication was armed for another save request");
        }
        let mut state = control.0.lock().map_err(|_| "Save state is unavailable")?;
        if !state.started
            || state.status.document_id != self.document
            || state.status.revision != self.revision
            || state.status.run != self.run
            || state.status.finished
            || !state.status.can_cancel
            || state.status.cancel_requested
        {
            return Err("Save publication lost cancellation or request ownership");
        }
        state.status.can_cancel = false;
        state.status.stage = self.stage;
        state.status.completed = 0;
        state.status.total = 0;
        drop(state);
        Ok(CommittedControl {
            control: self.control,
            document: self.document,
            run: self.run,
            revision: self.revision,
        })
    }
}
impl CommittedControl {
    pub(crate) fn check(&self) -> Result<(), String> {
        let state = self
            .control
            .0
            .lock()
            .map_err(|_| "Save state is unavailable")?;
        if !state.started
            || state.status.document_id != self.document
            || state.status.revision != self.revision
            || state.status.run != self.run
            || state.status.finished
            || state.status.can_cancel
            || state.status.cancel_requested
        {
            return Err("Committed save request no longer owns finalization".into());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prepared_publication_rejects_unstarted_foreign_finished_stale_and_reused_controls() {
        let control = Control::default();
        assert!(control.prepare_commit("source", 7).is_err());
        control.begin("source".into(), 7).unwrap();
        assert!(control.prepare_commit("other", 7).is_err());
        assert!(control.prepare_commit("source", 8).is_err());
        let prepared = control.prepare_commit("source", 7).unwrap();
        let foreign = Control::default();
        foreign.begin("source".into(), 7).unwrap();
        assert!(prepared.commit(&foreign).is_err());
        assert!(control.status().can_cancel);
        assert!(foreign.status().can_cancel);
        let prepared = control.prepare_commit("source", 7).unwrap();
        let stale = control.prepare_commit("source", 7).unwrap();
        let committed = prepared.commit(&control).unwrap();
        committed.check().unwrap();
        assert!(stale.commit(&control).is_err());
        assert!(control.prepare_commit("source", 7).is_err());
        assert!(!control.cancel());
        control.finish(&Ok(()));
        assert!(committed.check().is_err());
        assert!(control.prepare_commit("source", 7).is_err());
        let cancelled = Control::default();
        cancelled.begin("source".into(), 7).unwrap();
        let prepared = cancelled.prepare_commit("source", 7).unwrap();
        assert!(cancelled.cancel());
        assert!(prepared.commit(&cancelled).is_err());
    }

    #[test]
    fn prepared_commit_and_cancel_have_exactly_one_winner() {
        for _ in 0..32 {
            let control = Control::default();
            control.begin("source".into(), 7).unwrap();
            let prepared = control.prepare_commit("source", 7).unwrap();
            let barrier = Arc::new(std::sync::Barrier::new(2));
            let cancel = control.clone();
            let worker_barrier = barrier.clone();
            let worker = std::thread::spawn(move || {
                worker_barrier.wait();
                cancel.cancel()
            });
            barrier.wait();
            let committed = prepared.commit(&control).is_ok();
            assert_ne!(worker.join().unwrap(), committed);
            assert!(!control.status().can_cancel);
        }
    }

    #[test]
    fn publication_blocks_mutations_until_success_or_failure_without_blocking_reads() {
        for outcome in [Ok(()), Err("late publication verification failed".into())] {
            let control = Control::default();
            control.ensure_editable().unwrap();
            control.begin("source".into(), 17).unwrap();
            control.ensure_editable().unwrap();
            control.progress("Encoding", 12, 31).unwrap();
            control.ensure_editable().unwrap();
            control.commit().unwrap();
            assert!(control.ensure_editable().is_err());
            assert_eq!(control.status().revision, 17);
            control
                .progress("Verifying published bytes", 19, 31)
                .unwrap();
            assert!(control.ensure_editable().is_err());
            assert!(!control.cancel());
            control.finish(&outcome);
            control.ensure_editable().unwrap();
        }
        let cancelled = Control::default();
        cancelled.begin("source".into(), 17).unwrap();
        assert!(cancelled.cancel());
        cancelled.ensure_editable().unwrap();
        assert!(cancelled.commit().is_err());
        cancelled.ensure_editable().unwrap();
    }

    #[test]
    fn cancel_and_commit_have_one_winner_and_completed_controls_cannot_be_reused() {
        for cancel_first in [true, false] {
            let control = Control::default();
            control.begin("source".into(), 7).unwrap();
            if cancel_first {
                assert!(control.cancel());
                assert!(!control.cancel());
                assert!(control.commit().is_err());
                assert!(control.progress("Writing", 1, 3).is_err());
                control.finish(&control.check());
            } else {
                control.commit().unwrap();
                assert!(!control.cancel());
                assert!(!control.status().can_cancel);
                control.finish(&Ok(()));
            }
            assert!(control.status().finished);
            assert!(!control.cancel());
            assert!(control.begin("later".into(), 8).is_err());
            assert_eq!(control.status().document_id, "source");
            assert_eq!(control.status().revision, 7);
        }
    }

    #[test]
    fn racing_cancel_and_commit_never_both_succeed() {
        for _ in 0..32 {
            let control = Control::default();
            control.begin("source".into(), 0).unwrap();
            let barrier = Arc::new(std::sync::Barrier::new(2));
            let cancel = control.clone();
            let worker_barrier = barrier.clone();
            let worker = std::thread::spawn(move || {
                worker_barrier.wait();
                cancel.cancel()
            });
            barrier.wait();
            let committed = control.commit().is_ok();
            assert_ne!(worker.join().unwrap(), committed);
            assert!(!control.status().can_cancel);
        }
    }
}
