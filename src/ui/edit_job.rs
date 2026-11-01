//! One terminal edit at a time. Submission never reports an edit as committed.
use crate::psd::memory36::Admitted;
use serde_json::Value;
use std::sync::{mpsc, Arc};

#[derive(Clone)]
pub(super) enum Commands {
    Resident(Arc<Vec<Value>>),
    Native(Admitted<Value>),
}
impl Commands {
    pub(super) fn resident(commands: Vec<Value>) -> Self {
        Self::Resident(Arc::new(commands))
    }
    pub(super) fn values(&self) -> &[Value] {
        match self {
            Self::Resident(commands) => commands,
            Self::Native(command) => std::slice::from_ref(&**command),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Origin {
    Action,
    Source(u64),
}
pub(super) struct Request {
    pub project: String,
    pub document: String,
    pub revision: u64,
    pub label: String,
    pub selection_edit: bool,
    pub origin: Origin,
}
pub(super) struct Completed {
    pub request: Arc<Request>,
    pub result: Result<Value, String>,
    // Keep the admitted command allocation alive through terminal handling.
    _commands: Commands,
}
struct Pending {
    request: Arc<Request>,
    commands: Commands,
    result: mpsc::Receiver<Result<Value, String>>,
}
#[derive(Default)]
pub(super) struct Worker {
    pending: Option<Pending>,
}
impl Worker {
    pub(super) fn pending(&self) -> bool {
        self.pending.is_some()
    }
    pub(super) fn request(&self) -> Option<&Request> {
        self.pending.as_ref().map(|pending| &*pending.request)
    }
    pub(super) fn submit(
        &mut self,
        request: Request,
        commands: Commands,
        edit: impl FnOnce(&Request, &[Value]) -> Result<Value, String> + Send + 'static,
        repaint: impl FnOnce() + Send + 'static,
    ) -> Result<(), String> {
        if self.pending.is_some() {
            return Err("An edit is still being applied; wait for its result".into());
        }
        let request = Arc::new(request);
        let worker_request = request.clone();
        let worker_commands = commands.clone();
        let (tx, result) = mpsc::sync_channel(1);
        std::thread::Builder::new()
            .name("peerbrush-edit".into())
            .spawn(move || {
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    edit(&worker_request, worker_commands.values())
                }))
                .unwrap_or_else(|_| {
                    Err("Edit could not complete; refresh the document before retrying".into())
                });
                let _ = tx.send(result);
                repaint();
            })
            .map_err(|_| "Could not start the edit worker; draft preserved".to_string())?;
        self.pending = Some(Pending {
            request,
            commands,
            result,
        });
        Ok(())
    }
    pub(super) fn poll(&mut self) -> Option<Completed> {
        let pending = self.pending.as_ref()?;
        let result = match pending.result.try_recv() {
            Ok(result) => result,
            Err(mpsc::TryRecvError::Empty) => return None,
            Err(mpsc::TryRecvError::Disconnected) => {
                Err("Edit result was interrupted; refresh the document before retrying".into())
            }
        };
        let pending = self
            .pending
            .take()
            .expect("pending edit was observed above");
        Some(Completed {
            request: pending.request,
            result,
            _commands: pending.commands,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn request(origin: Origin) -> Request {
        Request {
            project: "project-a".into(),
            document: "document-a".into(),
            revision: 37,
            label: "Apply source".into(),
            selection_edit: false,
            origin,
        }
    }
    fn wait(worker: &mut Worker) -> Completed {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            if let Some(completed) = worker.poll() {
                return completed;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "worker did not finish"
            );
            std::thread::yield_now();
        }
    }
    #[test]
    fn pending_keeps_exact_identity_and_commands_until_terminal_success() {
        let mut worker = Worker::default();
        let commands = Commands::resident(vec![json!({"op":"paint.fill","color":[7,8,9,255]})]);
        let Commands::Resident(owner) = &commands else {
            unreachable!()
        };
        let observer = Arc::downgrade(owner);
        let (release, held) = mpsc::sync_channel(0);
        let main = std::thread::current().id();
        worker
            .submit(
                request(Origin::Source(12)),
                commands,
                move |request, commands| {
                    assert_ne!(std::thread::current().id(), main);
                    assert_eq!(
                        (
                            request.project.as_str(),
                            request.document.as_str(),
                            request.revision
                        ),
                        ("project-a", "document-a", 37)
                    );
                    assert_eq!(commands[0]["color"], json!([7, 8, 9, 255]));
                    held.recv().unwrap();
                    Ok(json!({"revision":38,"created":["new-layer"]}))
                },
                || {},
            )
            .unwrap();
        assert!(worker.pending());
        assert_eq!(worker.request().unwrap().origin, Origin::Source(12));
        assert!(worker.poll().is_none());
        assert!(worker
            .submit(
                request(Origin::Action),
                Commands::resident(vec![]),
                |_, _| Ok(Value::Null),
                || {}
            )
            .is_err());
        release.send(()).unwrap();
        let completed = wait(&mut worker);
        assert!(!worker.pending());
        assert_eq!(completed.result.as_ref().unwrap()["revision"], 38);
        assert!(observer.upgrade().is_some());
        drop(completed);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while observer.upgrade().is_some() {
            assert!(std::time::Instant::now() < deadline);
            std::thread::yield_now();
        }
    }
    #[test]
    fn native_command_admission_survives_worker_finish_and_terminal_handling() {
        use crate::{large_document::ResidentBudget, psd::memory36::MetadataCharge};
        let pool = ResidentBudget::new(8192).unwrap();
        let admission = MetadataCharge::reserve(&pool, 4096, &|| Ok(())).unwrap();
        let command = Admitted::from_parts(
            Arc::new(json!({"op":"source.update","text":"native draft"})),
            admission,
        );
        let (release, held) = mpsc::sync_channel(0);
        let (finished, finish) = mpsc::sync_channel(1);
        let mut worker = Worker::default();
        worker
            .submit(
                request(Origin::Source(43)),
                Commands::Native(command),
                move |_, commands| {
                    assert_eq!(commands[0]["text"], "native draft");
                    held.recv().unwrap();
                    Ok(json!({"revision":38}))
                },
                move || {
                    let _ = finished.send(());
                },
            )
            .unwrap();
        assert_eq!(pool.used(), 4096);
        release.send(()).unwrap();
        finish
            .recv_timeout(std::time::Duration::from_secs(5))
            .unwrap();
        assert_eq!(pool.used(), 4096);
        let completed = wait(&mut worker);
        assert_eq!(pool.used(), 4096);
        assert!(completed.result.is_ok());
        drop(completed);
        assert_eq!(pool.used(), 0);
    }
    #[test]
    fn refusal_and_panic_are_terminal_failures_and_never_report_queued_success() {
        let mut worker = Worker::default();
        let (repainted, repaint) = mpsc::sync_channel(1);
        worker
            .submit(
                request(Origin::Source(21)),
                Commands::resident(vec![]),
                |_, _| Err("stale document".into()),
                move || {
                    let _ = repainted.send(());
                },
            )
            .unwrap();
        let completed = wait(&mut worker);
        assert_eq!(completed.request.origin, Origin::Source(21));
        assert_eq!(completed.result.unwrap_err(), "stale document");
        worker
            .submit(
                request(Origin::Source(22)),
                Commands::resident(vec![]),
                |_, _| panic!("synthetic worker interruption"),
                || {},
            )
            .unwrap();
        let completed = wait(&mut worker);
        assert_eq!(completed.request.origin, Origin::Source(22));
        assert!(completed.result.unwrap_err().contains("refresh"));
        repaint
            .recv_timeout(std::time::Duration::from_secs(5))
            .unwrap();
    }
}
