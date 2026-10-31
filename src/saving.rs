//! Working-save progress and its one-way cancellation/publication boundary.
use std::sync::{Arc, Mutex};

#[derive(Clone)]
pub struct Control(Arc<Mutex<State>>);
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

#[cfg(test)]
mod tests {
    use super::*;

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
