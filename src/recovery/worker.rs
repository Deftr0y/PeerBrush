use std::{
    sync::{Arc, Condvar, Mutex},
    thread::{self, JoinHandle},
    time::Duration,
};

/// The instance owner joins this worker before releasing its writer lock.
pub(crate) struct Worker {
    stop: Arc<(Mutex<bool>, Condvar)>,
    thread: Option<JoinHandle<()>>,
}

impl Worker {
    pub(crate) fn start(
        workspace: crate::workspace::Registry,
        dir: std::path::PathBuf,
    ) -> std::io::Result<Self> {
        let mut previous = super::Versions::new();
        Self::spawn(Duration::from_secs(5), move || {
            if workspace.lock().unwrap().exiting {
                return false;
            }
            super::checkpoint(&workspace, &dir, &mut previous);
            true
        })
    }

    fn spawn(
        interval: Duration,
        mut task: impl FnMut() -> bool + Send + 'static,
    ) -> std::io::Result<Self> {
        let stop = Arc::new((Mutex::new(false), Condvar::new()));
        let waiting = stop.clone();
        let thread = thread::Builder::new()
            .name("peerbrush-recovery".into())
            .spawn(move || loop {
                let (state, wake) = &*waiting;
                let stopped = state.lock().unwrap_or_else(|e| e.into_inner());
                let (stopped, _) = wake
                    .wait_timeout_while(stopped, interval, |stop| !*stop)
                    .unwrap_or_else(|e| e.into_inner());
                if *stopped {
                    break;
                }
                drop(stopped);
                if !task() {
                    break;
                }
            })?;
        Ok(Self {
            stop,
            thread: Some(thread),
        })
    }

    pub(crate) fn stop(&mut self) {
        let (state, wake) = &*self.stop;
        *state.lock().unwrap_or_else(|e| e.into_inner()) = true;
        wake.notify_all();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    #[test]
    fn stopping_wakes_sleep_and_never_starts_another_task() {
        let (started, events) = mpsc::channel();
        let worker = Worker::spawn(Duration::from_secs(600), move || {
            started.send(()).unwrap();
            true
        })
        .unwrap();
        drop(worker);
        assert!(matches!(
            events.try_recv(),
            Err(mpsc::TryRecvError::Disconnected)
        ));
    }

    #[test]
    fn drop_waits_for_an_admitted_task_before_releasing_the_owner() {
        let (entered, enter) = mpsc::channel();
        let (resume, paused) = mpsc::channel();
        let worker = Worker::spawn(Duration::from_millis(1), move || {
            entered.send(()).unwrap();
            paused.recv().unwrap();
            true
        })
        .unwrap();
        enter.recv_timeout(Duration::from_secs(5)).unwrap();
        let (finished, done) = mpsc::channel();
        let dropper = thread::spawn(move || {
            drop(worker);
            finished.send(()).unwrap();
        });
        assert!(matches!(
            done.recv_timeout(Duration::from_millis(25)),
            Err(mpsc::RecvTimeoutError::Timeout)
        ));
        resume.send(()).unwrap();
        done.recv_timeout(Duration::from_secs(5)).unwrap();
        dropper.join().unwrap();
        assert!(matches!(
            enter.try_recv(),
            Err(mpsc::TryRecvError::Disconnected)
        ));
    }
}
