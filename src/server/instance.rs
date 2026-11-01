use super::{handle_http, Shared};
use std::{
    fs::File,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Condvar, Mutex, Weak,
    },
    thread::{self, JoinHandle},
};

#[derive(Default)]
struct State {
    stopping: bool,
    running: usize,
}

#[derive(Default)]
pub(super) struct Requests {
    state: Mutex<State>,
    idle: Condvar,
}

pub(super) struct Request(Arc<Requests>);

impl Requests {
    fn stopping(&self) -> bool {
        self.state
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .stopping
    }
    pub(super) fn begin(self: &Arc<Self>) -> Option<Request> {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.stopping {
            return None;
        }
        state.running = state.running.checked_add(1)?;
        Some(Request(self.clone()))
    }
    fn stop(&self) {
        self.state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .stopping = true;
    }
    fn drain(&self) {
        let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        drop(
            self.idle
                .wait_while(state, |state| state.running != 0)
                .unwrap_or_else(|e| e.into_inner()),
        );
    }
}

/// Source I/O lifetime admission, not authority over any filesystem path.
#[derive(Clone)]
pub(crate) struct SourceAdmission {
    requests: Arc<Requests>,
    workspace: Weak<Mutex<crate::workspace::Workspace>>,
}

pub(crate) struct SourceOperation {
    // Release the workspace counter before allowing instance drain to finish.
    _operation: super::FileOperation,
    request: Request,
    identity: Arc<()>,
}
/// One committed intent on one already-admitted operation. No new source I/O
/// or writer may be constructed from this value after the closing gate wins.
pub(crate) struct FinalizationPermit {
    operation: Arc<()>,
    intent: [u8; 32],
    saving: crate::saving::CommittedControl,
}

impl SourceAdmission {
    pub(crate) fn workspace(&self) -> Result<crate::workspace::Registry, String> {
        if self.requests.stopping() {
            return Err("PeerBrush source session is closing".into());
        }
        self.workspace
            .upgrade()
            .ok_or_else(|| "PeerBrush source workspace is unavailable".into())
    }
    pub(crate) fn begin(&self) -> Result<SourceOperation, String> {
        let request = self
            .requests
            .begin()
            .ok_or("PeerBrush source session is closing")?;
        let workspace = self.workspace()?;
        let operation = super::FileOperation::begin_workspace(&workspace)?;
        let permit = SourceOperation {
            _operation: operation,
            request,
            identity: Arc::new(()),
        };
        permit.check()?;
        Ok(permit)
    }
}

impl SourceOperation {
    /// Final memory-only publication while this exact request is still live.
    /// The closure must not perform I/O, call admission/check methods, or drop a
    /// final Request/SourceOperation owner (which reenters this state mutex).
    pub(crate) fn with_live_fence<T>(&self, commit: impl FnOnce() -> T) -> Result<T, String> {
        let state = self
            .request
            .0
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if state.stopping {
            return Err("PeerBrush source session is closing".into());
        }
        Ok(commit())
    }
    pub(crate) fn commit_finalization(
        &self,
        intent: [u8; 32],
        control: &crate::saving::Control,
        prepared: crate::saving::PreparedCommit,
    ) -> Result<FinalizationPermit, &'static str> {
        let state = self
            .request
            .0
            .state
            .lock()
            .map_err(|_| "Source request state is unavailable")?;
        if state.stopping || intent == [0; 32] {
            return Err("Source publication cannot commit after closing");
        }
        let saving = prepared.commit(control)?;
        let permit = FinalizationPermit {
            operation: self.identity.clone(),
            intent,
            saving,
        };
        drop(state);
        Ok(permit)
    }
    pub(crate) fn check_finalization(
        &self,
        permit: &FinalizationPermit,
        intent: [u8; 32],
    ) -> Result<(), String> {
        if !Arc::ptr_eq(&self.identity, &permit.operation) || permit.intent != intent {
            return Err("Finalization belongs to another source operation or intent".into());
        }
        permit.saving.check()
    }
    pub(crate) fn check(&self) -> Result<(), String> {
        if self.request.0.stopping() {
            Err("PeerBrush source session is closing".into())
        } else {
            Ok(())
        }
    }
}

impl Drop for Request {
    fn drop(&mut self) {
        let mut state = self.0.state.lock().unwrap_or_else(|e| e.into_inner());
        state.running -= 1;
        if state.running == 0 {
            self.0.idle.notify_all();
        }
    }
}

/// Owns the exact instance file until all admitted network/recovery writers end.
/// It is never cloned or transferred to a worker.
pub struct InstanceLock {
    requests: Arc<Requests>,
    availability: Arc<AtomicBool>,
    workspace: Weak<Mutex<crate::workspace::Workspace>>,
    recovery: crate::recovery::Worker,
    listener: Option<JoinHandle<()>>,
    http: Arc<tiny_http::Server>,
    // Field order also closes the listener before releasing writer admission.
    _file: File,
}

impl InstanceLock {
    pub(crate) fn source_admission_for(
        &self,
        dir: &std::path::Path,
    ) -> Result<(SourceAdmission, (u64, u64)), String> {
        let named = crate::file_safety::open_for_identity(&dir.join("instance.lock"))
            .map_err(|_| "Source connection lock is unavailable")?;
        if crate::file_safety::identity(&named)? != crate::file_safety::identity(&self._file)?
            || crate::file_safety::link_count(&named)? != 1
            || crate::file_safety::link_count(&self._file)? != 1
        {
            return Err("Source connection differs from its actual instance lock".into());
        }
        Ok((
            self.source_admission(),
            crate::file_safety::identity(&self._file)?,
        ))
    }
    pub(crate) fn source_admission(&self) -> SourceAdmission {
        SourceAdmission {
            requests: self.requests.clone(),
            workspace: self.workspace.clone(),
        }
    }
    pub(super) fn start(
        file: File,
        http: tiny_http::Server,
        shared: Shared,
        workspace: crate::workspace::Registry,
        dir: std::path::PathBuf,
        auth: String,
        port: u16,
        availability: Arc<AtomicBool>,
    ) -> Result<Self, String> {
        let source_workspace = Arc::downgrade(&workspace);
        let recovery = crate::recovery::Worker::start(workspace, dir).map_err(|e| e.to_string())?;
        let requests = Arc::new(Requests::default());
        let accepting = requests.clone();
        let http = Arc::new(http);
        let incoming = http.clone();
        let listening = availability.clone();
        let listener = thread::Builder::new()
            .name("peerbrush-http".into())
            .spawn(move || {
                let _availability = super::ServerAvailability(listening);
                let mut sessions = std::collections::BTreeMap::new();
                while let Ok(request) = incoming.recv() {
                    let Some(_request) = accepting.begin() else {
                        break;
                    };
                    handle_http(request, &shared, &auth, port, &mut sessions, &accepting);
                }
            })
            .map_err(|e| e.to_string())?;
        Ok(Self {
            requests,
            availability,
            workspace: source_workspace,
            recovery,
            listener: Some(listener),
            http,
            _file: file,
        })
    }
}

impl Drop for InstanceLock {
    fn drop(&mut self) {
        self.requests.stop();
        self.availability.store(false, Ordering::Release);
        self.http.unblock();
        self.recovery.stop();
        if let Some(listener) = self.listener.take() {
            let _ = listener.join();
        }
        self.requests.drain();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn source_finalization_is_exact_operation_intent_and_cannot_be_reused() {
        let shared = Arc::new(Mutex::new(crate::engine::Engine::new()));
        let workspace = crate::workspace::attach(&shared);
        let requests = Arc::new(Requests::default());
        let admission = SourceAdmission {
            requests: requests.clone(),
            workspace: Arc::downgrade(&workspace),
        };
        let first = admission.begin().unwrap();
        let foreign = admission.begin().unwrap();
        let control = crate::saving::Control::default();
        control.begin("source".into(), 7).unwrap();
        let prepared = control.prepare_commit("source", 7).unwrap();
        let stale = control.prepare_commit("source", 7).unwrap();
        let permit = first
            .commit_finalization([1; 32], &control, prepared)
            .unwrap();
        first.check_finalization(&permit, [1; 32]).unwrap();
        assert!(foreign.check_finalization(&permit, [1; 32]).is_err());
        assert!(first.check_finalization(&permit, [2; 32]).is_err());
        assert!(first.commit_finalization([1; 32], &control, stale).is_err());
        requests.stop();
        assert!(first.check().is_err());
        assert!(admission.begin().is_err());
        first.check_finalization(&permit, [1; 32]).unwrap();
        control.finish(&Ok(()));
        assert!(first.check_finalization(&permit, [1; 32]).is_err());
    }

    #[test]
    fn source_closing_and_prepared_commit_serialize_before_the_one_way_save_boundary() {
        for _ in 0..32 {
            let shared = Arc::new(Mutex::new(crate::engine::Engine::new()));
            let workspace = crate::workspace::attach(&shared);
            let requests = Arc::new(Requests::default());
            let admission = SourceAdmission {
                requests: requests.clone(),
                workspace: Arc::downgrade(&workspace),
            };
            let operation = admission.begin().unwrap();
            let control = crate::saving::Control::default();
            control.begin("source".into(), 7).unwrap();
            let prepared = control.prepare_commit("source", 7).unwrap();
            let barrier = Arc::new(std::sync::Barrier::new(2));
            let stop = requests.clone();
            let other = barrier.clone();
            let worker = thread::spawn(move || {
                other.wait();
                stop.stop()
            });
            barrier.wait();
            let result = operation.commit_finalization([1; 32], &control, prepared);
            worker.join().unwrap();
            match result {
                Ok(permit) => {
                    assert!(!control.status().can_cancel);
                    operation.check_finalization(&permit, [1; 32]).unwrap();
                    assert!(!control.cancel());
                }
                Err(_) => {
                    assert!(control.status().can_cancel);
                    assert!(control.cancel());
                }
            }
            assert!(operation.check().is_err());
            assert!(admission.begin().is_err());
        }
    }
    use std::{
        fs,
        io::{Read, Write},
        net::TcpStream,
        path::PathBuf,
        sync::mpsc,
        time::{Duration, Instant},
    };

    fn directory() -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("peerbrush-instance-tests-{}", crate::engine::id()));
        fs::create_dir(&dir).unwrap();
        fs::canonicalize(dir).unwrap()
    }

    fn cleanup(dir: &std::path::Path) {
        let temp = fs::canonicalize(std::env::temp_dir()).unwrap();
        assert!(dir.is_absolute() && dir.starts_with(temp));
        assert!(dir
            .file_name()
            .unwrap()
            .to_string_lossy()
            .starts_with("peerbrush-instance-tests-"));
        assert_eq!(fs::canonicalize(dir).unwrap(), dir);
        assert!(!fs::symlink_metadata(dir).unwrap().file_type().is_symlink());
        fs::remove_dir_all(dir).unwrap();
    }

    fn until(mut ready: impl FnMut() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !ready() {
            assert!(
                Instant::now() < deadline,
                "Instance test did not reach its controlled boundary"
            );
            thread::sleep(Duration::from_millis(1));
        }
    }

    fn lock(dir: &std::path::Path) -> File {
        fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(dir.join("instance.lock"))
            .unwrap()
    }

    fn save_brush() -> serde_json::Value {
        json!({"method":"brushes","params":{"action":"save","name":"Preserved admitted work","category":"Custom","settings":crate::brush::Settings::default()}})
    }

    #[test]
    fn stop_refuses_new_requests_and_drains_admitted_children() {
        let requests = Arc::new(Requests::default());
        let unrelated = Arc::new(Requests::default());
        let request = requests.begin().unwrap();
        let child = requests.begin().unwrap();
        requests.stop();
        assert!(requests.begin().is_none());
        assert!(unrelated.begin().is_some());
        let stopping = requests.clone();
        let (finished, done) = mpsc::channel();
        let wait = thread::spawn(move || {
            stopping.drain();
            finished.send(()).unwrap();
        });
        drop(request);
        assert!(matches!(
            done.recv_timeout(Duration::from_millis(25)),
            Err(mpsc::RecvTimeoutError::Timeout)
        ));
        drop(child);
        done.recv_timeout(Duration::from_secs(5)).unwrap();
        wait.join().unwrap();
        assert!(requests.begin().is_none());
    }

    #[test]
    fn connection_drop_waits_for_actual_profile_writer_before_unlock_and_stops_old_rpc() {
        let dir = directory();
        let shared = Arc::new(Mutex::new(crate::engine::Engine::new()));
        let connection = super::super::start(shared.clone(), dir.clone()).unwrap();
        let workspace = crate::workspace::attach(&shared);
        let library = shared.lock().unwrap().brush_library.clone();
        let held = library.lock().unwrap();
        let request_dir = dir.clone();
        let request =
            thread::spawn(move || super::super::client(&request_dir, "rpc", &save_brush()));
        until(|| {
            workspace
                .lock()
                .unwrap()
                .file_operations
                .load(Ordering::Acquire)
                != 0
        });
        let old_port = connection.port;
        let old_token = connection.token.clone();
        let (finished, done) = mpsc::channel();
        let dropper = thread::spawn(move || {
            drop(connection);
            finished.send(()).unwrap();
        });
        assert!(matches!(
            done.recv_timeout(Duration::from_millis(25)),
            Err(mpsc::RecvTimeoutError::Timeout)
        ));
        assert!(
            lock(&dir).try_lock().is_err(),
            "Writer admission must remain held while profile I/O is pending"
        );
        drop(held);
        assert_eq!(request.join().unwrap().unwrap()["ok"], true);
        done.recv_timeout(Duration::from_secs(5)).unwrap();
        dropper.join().unwrap();
        let next_owner = lock(&dir);
        next_owner.try_lock().unwrap();
        let before = fs::read(dir.join("brushes.json")).unwrap();
        assert!(matches!(
            ureq::post(&format!("http://127.0.0.1:{old_port}/rpc"))
                .set("Authorization", &format!("Bearer {old_token}"))
                .timeout(Duration::from_secs(1))
                .send_json(save_brush()),
            Err(ureq::Error::Transport(_))
        ));
        assert_eq!(fs::read(dir.join("brushes.json")).unwrap(), before);
        drop(next_owner);
        cleanup(&dir);
    }

    #[test]
    fn parsed_request_cannot_start_a_candidate_after_shutdown_admission_closes() {
        let dir = directory();
        let shared = Arc::new(Mutex::new(crate::engine::Engine::new()));
        let connection = super::super::start(shared.clone(), dir.clone()).unwrap();
        let requests = connection.instance_lock.as_ref().unwrap().requests.clone();
        let mut body = serde_json::to_vec(&save_brush()).unwrap();
        // tiny_http buffers bodies <=1024 bytes before exposing their Request.
        body.resize(2048, b' ');
        let mut stream = TcpStream::connect(("127.0.0.1", connection.port)).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        stream
            .set_write_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        write!(stream, "POST /rpc HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nAuthorization: Bearer {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", connection.port, connection.token, body.len()).unwrap();
        stream.write_all(&body[..1]).unwrap();
        until(|| requests.state.lock().unwrap().running == 1);
        let dropper = thread::spawn(move || drop(connection));
        until(|| requests.state.lock().unwrap().stopping);
        assert!(lock(&dir).try_lock().is_err());
        stream.write_all(&body[1..]).unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).unwrap();
        assert!(response.starts_with("HTTP/1.1 503"), "{response}");
        dropper.join().unwrap();
        assert!(!dir.join("brushes.json").exists());
        assert!(shared.lock().unwrap().undo.is_empty());
        let next_owner = lock(&dir);
        next_owner.try_lock().unwrap();
        drop(next_owner);
        cleanup(&dir);
    }

    #[test]
    fn update_seal_preserves_writer_admission_until_connection_quiesces() {
        let dir = directory();
        let shared = Arc::new(Mutex::new(crate::engine::Engine::new()));
        let connection = super::super::start(shared.clone(), dir.clone()).unwrap();
        let workspace = crate::workspace::attach(&shared);
        let reviewed = crate::workspace::lifecycle::update_readiness(&workspace).unwrap();
        crate::workspace::lifecycle::finish_update_exit(&workspace, &reviewed).unwrap();
        assert!(lock(&dir).try_lock().is_err());
        super::super::checkpoint_projects(
            &workspace,
            &dir,
            &mut super::super::RecoveryVersions::new(),
        );
        assert!(crate::recovery::catalog(&dir).is_empty());
        let reply = super::super::client(&dir, "rpc", &save_brush()).unwrap();
        assert_eq!(reply["ok"], false);
        assert!(!dir.join("brushes.json").exists());
        drop(connection);
        let next_owner = lock(&dir);
        next_owner.try_lock().unwrap();
        drop(next_owner);
        cleanup(&dir);
    }

    #[test]
    fn source_configuration_is_bound_once_and_stays_lazy_after_instance_shutdown() {
        let dir = directory();
        let shared = Arc::new(Mutex::new(crate::engine::Engine::new()));
        let connection = super::super::start(shared.clone(), dir.clone()).unwrap();
        let workspace = crate::workspace::attach(&shared);
        assert!(connection.configure_native_sources(None, false).is_err());
        assert!(workspace.lock().unwrap().source_runtime.is_none());
        connection.configure_native_sources(None, true).unwrap();
        let runtime = workspace
            .lock()
            .unwrap()
            .source_runtime
            .as_ref()
            .unwrap()
            .clone();
        assert_eq!(runtime.resident().used(), 0);
        assert_eq!(runtime.disk().used().unwrap(), 0);
        assert!(!dir.join("native-sources-v1").exists());
        assert!(connection.configure_native_sources(None, true).is_err());
        assert!(Arc::ptr_eq(
            &runtime,
            workspace.lock().unwrap().source_runtime.as_ref().unwrap()
        ));
        let other = Arc::new(Mutex::new(crate::engine::Engine::new()));
        assert!(crate::workspace::attach(&other)
            .lock()
            .unwrap()
            .source_runtime
            .is_none());
        drop(connection);
        assert!(runtime.begin().is_err());
        assert!(!dir.join("native-sources-v1").exists());
        let registered = workspace.lock().unwrap().source_runtime.take();
        drop(registered);
        drop(runtime);
        drop(workspace);
        drop(shared);
        drop(other);
        cleanup(&dir);
    }

    #[test]
    fn source_operation_blocks_update_seal_and_cannot_begin_after_seal() {
        let dir = directory();
        let shared = Arc::new(Mutex::new(crate::engine::Engine::new()));
        let connection = super::super::start(shared.clone(), dir.clone()).unwrap();
        let workspace = crate::workspace::attach(&shared);
        let reviewed = crate::workspace::lifecycle::update_readiness(&workspace).unwrap();
        let source = connection
            .instance_lock
            .as_ref()
            .unwrap()
            .source_admission();
        let operation = source.begin().unwrap();
        assert_eq!(
            workspace
                .lock()
                .unwrap()
                .file_operations
                .load(Ordering::Acquire),
            1
        );
        assert!(crate::workspace::lifecycle::finish_update_exit(&workspace, &reviewed).is_err());
        assert!(!shared.lock().unwrap().closed);
        drop(operation);
        crate::workspace::lifecycle::finish_update_exit(&workspace, &reviewed).unwrap();
        assert!(source.begin().is_err());
        assert_eq!(
            workspace
                .lock()
                .unwrap()
                .file_operations
                .load(Ordering::Acquire),
            0
        );
        assert!(!dir.join("sources").exists());
        drop(connection);
        cleanup(&dir);
    }

    #[test]
    fn instance_drop_waits_for_source_operation_without_cloning_its_file() {
        let dir = directory();
        let shared = Arc::new(Mutex::new(crate::engine::Engine::new()));
        let connection = super::super::start(shared, dir.clone()).unwrap();
        let source = connection
            .instance_lock
            .as_ref()
            .unwrap()
            .source_admission();
        let operation = source.begin().unwrap();
        let (finished, done) = mpsc::channel();
        let dropper = thread::spawn(move || {
            drop(connection);
            finished.send(()).unwrap();
        });
        until(|| source.requests.stopping());
        assert!(source.begin().is_err());
        assert!(operation.check().is_err());
        assert!(matches!(
            done.recv_timeout(Duration::from_millis(25)),
            Err(mpsc::RecvTimeoutError::Timeout)
        ));
        assert!(lock(&dir).try_lock().is_err());
        drop(operation);
        done.recv_timeout(Duration::from_secs(5)).unwrap();
        dropper.join().unwrap();
        let next_owner = lock(&dir);
        next_owner.try_lock().unwrap();
        drop(next_owner);
        cleanup(&dir);
    }
}
