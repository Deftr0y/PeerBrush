//! One active preview and one newest queued request retain bounded source owners.
use std::{
    io,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Condvar, Mutex,
    },
    thread::{self, JoinHandle},
};

struct LegacyJob {
    render: Box<dyn FnOnce() + Send>,
    failed: Box<dyn FnOnce() + Send>,
}
enum Job {
    Legacy(LegacyJob),
    Native {
        request: crate::render::native_preview::Request,
        job: u64,
        epoch: Arc<AtomicU64>,
        replies: NativeReplies,
        ready: Box<dyn FnOnce() + Send>,
        uploads: Option<(super::native_texture::Retirement, usize)>,
    },
}

pub(super) struct NativeReply {
    pub job: u64,
    pub tag: crate::render::native_preview::Tag,
    pub result: Result<NativeOutput, String>,
}
pub(super) enum NativeOutput {
    Pixels(crate::render::native_preview::Frame),
    Uploads(super::native_texture::Prepared),
}
/// At most one completed pixel-only frame is retained while UI is busy.
#[derive(Clone, Default)]
pub(super) struct NativeReplies(Arc<Mutex<Option<NativeReply>>>);
impl NativeReplies {
    pub fn take(&self) -> Option<NativeReply> {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).take()
    }
    pub(super) fn complete(&self, reply: NativeReply) {
        let previous = self
            .0
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .replace(reply);
        drop(previous);
    }
}

#[derive(Default)]
struct State {
    next: Option<Job>,
    stopped: bool,
}

#[derive(Default)]
struct Queue {
    state: Mutex<State>,
    ready: Condvar,
}

pub(super) struct Worker {
    queue: Arc<Queue>,
    epoch: Arc<AtomicU64>,
    thread: Option<JoinHandle<()>>,
}

impl Worker {
    pub fn new(epoch: Arc<AtomicU64>) -> Self {
        Self {
            queue: Arc::new(Queue::default()),
            epoch,
            thread: None,
        }
    }

    /// Jobs keep the existing epoch/source guards and send their terminal reply.
    /// A render panic invokes `failed`; thread creation failure returns to UI.
    pub fn request(
        &mut self,
        render: impl FnOnce() + Send + 'static,
        failed: impl FnOnce() + Send + 'static,
    ) -> io::Result<()> {
        self.enqueue(
            Job::Legacy(LegacyJob {
                render: Box::new(render),
                failed: Box::new(failed),
            }),
            |queue| {
                thread::Builder::new()
                    .name("peerbrush-preview".into())
                    .spawn(move || run(queue))
            },
        )
    }

    /// Request is already captured/admitted outside UI and Engine locks.
    pub fn request_native(
        &mut self,
        request: crate::render::native_preview::Request,
        job: u64,
        replies: NativeReplies,
        ready: impl FnOnce() + Send + 'static,
    ) -> io::Result<()> {
        self.enqueue(
            Job::Native {
                request,
                job,
                epoch: self.epoch.clone(),
                replies,
                ready: Box::new(ready),
                uploads: None,
            },
            |queue| {
                thread::Builder::new()
                    .name("peerbrush-preview".into())
                    .spawn(move || run(queue))
            },
        )
    }

    pub fn request_native_uploads(
        &mut self,
        request: crate::render::native_preview::Request,
        job: u64,
        replies: NativeReplies,
        retirement: super::native_texture::Retirement,
        max_texture_side: usize,
        ready: impl FnOnce() + Send + 'static,
    ) -> io::Result<()> {
        self.enqueue(
            Job::Native {
                request,
                job,
                epoch: self.epoch.clone(),
                replies,
                ready: Box::new(ready),
                uploads: Some((retirement, max_texture_side)),
            },
            |queue| {
                thread::Builder::new()
                    .name("peerbrush-preview".into())
                    .spawn(move || run(queue))
            },
        )
    }

    fn enqueue(
        &mut self,
        job: Job,
        spawn: impl FnOnce(Arc<Queue>) -> io::Result<JoinHandle<()>>,
    ) -> io::Result<()> {
        if self
            .queue
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .stopped
        {
            return Err(io::Error::other("Preview worker has stopped"));
        }
        if self.thread.is_none() {
            self.thread = Some(spawn(self.queue.clone())?);
        }
        let replaced = {
            let mut state = self.queue.state.lock().unwrap_or_else(|e| e.into_inner());
            if state.stopped {
                return Err(io::Error::other("Preview worker has stopped"));
            }
            state.next.replace(job)
        };
        self.queue.ready.notify_one();
        // Documents, native source owners and failure closures may have costly
        // destructors. Neither the worker nor another request waits on that lock.
        drop(replaced);
        Ok(())
    }

    /// Drain source owners before final profile observations or app teardown.
    pub fn stop(&mut self) {
        let queued = {
            let mut state = self.queue.state.lock().unwrap_or_else(|e| e.into_inner());
            if !state.stopped {
                state.stopped = true;
                self.epoch.fetch_add(1, Ordering::Relaxed);
            }
            state.next.take()
        };
        self.queue.ready.notify_one();
        drop(queued);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn run(queue: Arc<Queue>) {
    loop {
        let job = {
            let mut state = queue.state.lock().unwrap_or_else(|e| e.into_inner());
            while !state.stopped && state.next.is_none() {
                state = queue.ready.wait(state).unwrap_or_else(|e| e.into_inner());
            }
            if state.stopped {
                break;
            }
            state.next.take().unwrap()
        };
        match job {
            Job::Legacy(LegacyJob { render, failed }) => {
                if std::panic::catch_unwind(std::panic::AssertUnwindSafe(render)).is_err() {
                    // A reply sink must not kill the actor and strand the newest job.
                    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(failed));
                }
            }
            Job::Native {
                request,
                job,
                epoch,
                replies,
                ready,
                uploads,
            } => {
                let tag = request.tag();
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    request.render(&mut || {
                        if epoch.load(Ordering::Relaxed) == job {
                            Ok(())
                        } else {
                            Err("Preview superseded".into())
                        }
                    })
                }))
                .unwrap_or_else(|_| Err("Native preview could not complete".into()));
                // No scene, Surface, source session or profile operation escapes
                // into the UI reply. Release all of them before notifying UI.
                drop(request);
                let mut result = result.and_then(|frame| {
                    if let Some((retirement, side)) = uploads {
                        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                            super::native_texture::Prepared::from_frame(
                                frame,
                                &retirement,
                                side,
                                &mut || {
                                    if epoch.load(Ordering::Relaxed) == job {
                                        Ok(())
                                    } else {
                                        Err("Preview superseded".into())
                                    }
                                },
                            )
                            .map(NativeOutput::Uploads)
                        }))
                        .unwrap_or_else(|_| {
                            Err("Native display upload preparation could not complete".into())
                        })
                    } else {
                        Ok(NativeOutput::Pixels(frame))
                    }
                });
                if epoch.load(Ordering::Relaxed) != job {
                    result = Err("Preview superseded".into());
                }
                replies.complete(NativeReply { job, tag, result });
                let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(ready));
            }
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
    use std::{
        sync::{
            atomic::{AtomicBool, AtomicUsize},
            mpsc, Weak,
        },
        time::{Duration, Instant},
    };

    const WAIT: Duration = Duration::from_secs(3);

    struct Owner {
        id: usize,
        queue: Weak<Queue>,
        dropped: mpsc::Sender<(usize, bool)>,
    }
    impl Drop for Owner {
        fn drop(&mut self) {
            let unlocked = self
                .queue
                .upgrade()
                .is_none_or(|queue| queue.state.try_lock().is_ok());
            let _ = self.dropped.send((self.id, unlocked));
        }
    }

    #[test]
    fn one_active_and_only_newest_queued_job_keep_and_drop_owners_outside_queue_lock() {
        let epoch = Arc::new(AtomicU64::new(1));
        let mut worker = Worker::new(epoch);
        let (started_tx, started_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let (ran_tx, ran_rx) = mpsc::channel();
        let (drop_tx, drop_rx) = mpsc::channel();
        let active = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));
        let count = active.clone();
        let high = peak.clone();
        worker
            .request(
                move || {
                    high.fetch_max(count.fetch_add(1, Ordering::Relaxed) + 1, Ordering::Relaxed);
                    started_tx.send(()).unwrap();
                    release_rx.recv_timeout(WAIT).unwrap();
                    count.fetch_sub(1, Ordering::Relaxed);
                },
                || panic!("Active preview unexpectedly failed"),
            )
            .unwrap();
        started_rx.recv_timeout(WAIT).unwrap();
        for id in 2..=101 {
            let owner = Owner {
                id,
                queue: Arc::downgrade(&worker.queue),
                dropped: drop_tx.clone(),
            };
            let ran = ran_tx.clone();
            let count = active.clone();
            let high = peak.clone();
            worker
                .request(
                    move || {
                        high.fetch_max(
                            count.fetch_add(1, Ordering::Relaxed) + 1,
                            Ordering::Relaxed,
                        );
                        ran.send(id).unwrap();
                        drop(owner);
                        count.fetch_sub(1, Ordering::Relaxed);
                    },
                    || panic!("Queued preview unexpectedly failed"),
                )
                .unwrap();
            assert!(worker.queue.state.lock().unwrap().next.is_some());
            if id > 2 {
                assert_eq!(drop_rx.recv_timeout(WAIT).unwrap(), (id - 1, true));
            }
        }
        assert!(matches!(ran_rx.try_recv(), Err(mpsc::TryRecvError::Empty)));
        release_tx.send(()).unwrap();
        assert_eq!(ran_rx.recv_timeout(WAIT).unwrap(), 101);
        assert_eq!(drop_rx.recv_timeout(WAIT).unwrap(), (101, true));
        drop(worker);
        assert_eq!(peak.load(Ordering::Relaxed), 1);
        assert_eq!(active.load(Ordering::Relaxed), 0);
        assert!(matches!(ran_rx.try_recv(), Err(mpsc::TryRecvError::Empty)));
    }

    #[test]
    fn existing_epoch_cancels_active_render_before_the_latest_job_runs() {
        let epoch = Arc::new(AtomicU64::new(1));
        let mut worker = Worker::new(epoch.clone());
        let (start_tx, start_rx) = mpsc::channel();
        let (reply_tx, reply_rx) = mpsc::channel();
        let observed = epoch.clone();
        let stale_reply = reply_tx.clone();
        worker
            .request(
                move || {
                    start_tx.send(()).unwrap();
                    let deadline = Instant::now() + WAIT;
                    while observed.load(Ordering::Relaxed) == 1 {
                        assert!(Instant::now() < deadline);
                        thread::yield_now();
                    }
                    stale_reply.send("cancelled").unwrap();
                },
                || panic!("Epoch cancellation failed"),
            )
            .unwrap();
        start_rx.recv_timeout(WAIT).unwrap();
        epoch.store(2, Ordering::Relaxed);
        let observed = epoch.clone();
        worker
            .request(
                move || {
                    assert_eq!(observed.load(Ordering::Relaxed), 2);
                    reply_tx.send("latest").unwrap();
                },
                || panic!("Latest render failed"),
            )
            .unwrap();
        assert_eq!(reply_rx.recv_timeout(WAIT).unwrap(), "cancelled");
        assert_eq!(reply_rx.recv_timeout(WAIT).unwrap(), "latest");
    }

    #[test]
    fn stop_cancels_and_drains_before_later_source_or_profile_owners_drop() {
        struct Profile {
            active: Arc<AtomicBool>,
            queued: Arc<AtomicBool>,
        }
        impl Drop for Profile {
            fn drop(&mut self) {
                assert!(self.active.load(Ordering::Acquire));
                assert!(self.queued.load(Ordering::Acquire));
            }
        }
        struct App {
            worker: Worker,
            _profile: Profile,
        }
        struct Queued(Arc<AtomicBool>);
        impl Drop for Queued {
            fn drop(&mut self) {
                self.0.store(true, Ordering::Release);
            }
        }
        let epoch = Arc::new(AtomicU64::new(7));
        let active = Arc::new(AtomicBool::new(false));
        let queued = Arc::new(AtomicBool::new(false));
        let mut app = App {
            worker: Worker::new(epoch.clone()),
            _profile: Profile {
                active: active.clone(),
                queued: queued.clone(),
            },
        };
        let (start_tx, start_rx) = mpsc::channel();
        let observed = epoch.clone();
        let completed = active.clone();
        app.worker
            .request(
                move || {
                    start_tx.send(()).unwrap();
                    let deadline = Instant::now() + WAIT;
                    while observed.load(Ordering::Relaxed) == 7 {
                        assert!(Instant::now() < deadline);
                        thread::yield_now();
                    }
                    completed.store(true, Ordering::Release);
                },
                || panic!("Stop did not cancel active preview"),
            )
            .unwrap();
        start_rx.recv_timeout(WAIT).unwrap();
        let queued_owner = Queued(queued.clone());
        let ran = Arc::new(AtomicBool::new(false));
        let executed = ran.clone();
        app.worker
            .request(
                move || {
                    executed.store(true, Ordering::Release);
                    drop(queued_owner);
                    panic!("Stop executed an obsolete queued job");
                },
                || panic!("Queued job must be dropped, not executed"),
            )
            .unwrap();
        drop(app);
        assert_eq!(epoch.load(Ordering::Relaxed), 8);
        assert!(active.load(Ordering::Acquire) && queued.load(Ordering::Acquire));
        assert!(!ran.load(Ordering::Acquire));
    }

    #[test]
    fn render_and_failure_sink_panics_do_not_strand_the_newest_request() {
        let epoch = Arc::new(AtomicU64::new(1));
        let mut worker = Worker::new(epoch);
        let (start_tx, start_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let (failed_tx, failed_rx) = mpsc::channel();
        let (done_tx, done_rx) = mpsc::channel();
        worker
            .request(
                move || {
                    start_tx.send(thread::current().id()).unwrap();
                    release_rx.recv_timeout(WAIT).unwrap();
                    panic!("Synthetic render failure");
                },
                move || {
                    failed_tx.send(thread::current().id()).unwrap();
                    panic!("Synthetic failed sink");
                },
            )
            .unwrap();
        let thread = start_rx.recv_timeout(WAIT).unwrap();
        worker
            .request(
                move || done_tx.send(thread::current().id()).unwrap(),
                || panic!("Second render failed"),
            )
            .unwrap();
        release_tx.send(()).unwrap();
        assert_eq!(failed_rx.recv_timeout(WAIT).unwrap(), thread);
        assert_eq!(done_rx.recv_timeout(WAIT).unwrap(), thread);
    }

    #[test]
    fn failed_thread_creation_releases_incoming_owners_and_allows_a_later_request() {
        let epoch = Arc::new(AtomicU64::new(1));
        let mut worker = Worker::new(epoch);
        let (drop_tx, drop_rx) = mpsc::channel();
        let owner = Owner {
            id: 1,
            queue: Arc::downgrade(&worker.queue),
            dropped: drop_tx,
        };
        let failure = worker.enqueue(
            Job::Legacy(LegacyJob {
                render: Box::new(move || drop(owner)),
                failed: Box::new(|| panic!("UI handles thread creation failure")),
            }),
            |_| Err(io::Error::other("Synthetic thread creation failure")),
        );
        assert!(failure.is_err());
        assert!(worker.thread.is_none());
        assert!(worker.queue.state.lock().unwrap().next.is_none());
        assert_eq!(drop_rx.recv_timeout(WAIT).unwrap(), (1, true));
        let (done_tx, done_rx) = mpsc::channel();
        worker
            .request(
                move || done_tx.send(()).unwrap(),
                || panic!("Restart render failed"),
            )
            .unwrap();
        done_rx.recv_timeout(WAIT).unwrap();
    }

    #[test]
    fn explicit_stop_drains_stale_source_operations_once_before_final_profile_observation() {
        struct SourceOwner(Arc<AtomicUsize>);
        impl SourceOwner {
            fn new(count: &Arc<AtomicUsize>) -> Self {
                count.fetch_add(1, Ordering::Relaxed);
                Self(count.clone())
            }
        }
        impl Drop for SourceOwner {
            fn drop(&mut self) {
                self.0.fetch_sub(1, Ordering::Release);
            }
        }
        let epoch = Arc::new(AtomicU64::new(1));
        let sources = Arc::new(AtomicUsize::new(0));
        let mut worker = Worker::new(epoch.clone());
        let (started_tx, started_rx) = mpsc::channel();
        let active = SourceOwner::new(&sources);
        let observed = epoch.clone();
        worker
            .request(
                move || {
                    let _source = active;
                    started_tx.send(()).unwrap();
                    let deadline = Instant::now() + WAIT;
                    // Model a render stage retaining its source through an earlier
                    // invalidation until its next cancellation checkpoint at shutdown.
                    while observed.load(Ordering::Relaxed) < 3 {
                        assert!(Instant::now() < deadline);
                        thread::yield_now();
                    }
                },
                || panic!("Active source did not reach its stop checkpoint"),
            )
            .unwrap();
        started_rx.recv_timeout(WAIT).unwrap();
        epoch.store(2, Ordering::Relaxed);
        let queued = SourceOwner::new(&sources);
        let ran = Arc::new(AtomicBool::new(false));
        let executed = ran.clone();
        worker
            .request(
                move || {
                    executed.store(true, Ordering::Release);
                    drop(queued);
                },
                || panic!("Queued source failed"),
            )
            .unwrap();
        assert_eq!(sources.load(Ordering::Acquire), 2);
        worker.stop();
        // This is the point where on_exit may inspect/seal the profile.
        assert_eq!(sources.load(Ordering::Acquire), 0);
        assert!(!ran.load(Ordering::Acquire));
        assert!(worker.thread.is_none());
        assert_eq!(epoch.load(Ordering::Relaxed), 3);
        worker.stop();
        assert_eq!(epoch.load(Ordering::Relaxed), 3);
        let refused = SourceOwner::new(&sources);
        assert!(worker
            .request(
                move || drop(refused),
                || panic!("Stopped requests report failure to UI")
            )
            .is_err());
        assert!(worker.thread.is_none());
        assert_eq!(sources.load(Ordering::Acquire), 0);
        drop(worker);
        assert_eq!(epoch.load(Ordering::Relaxed), 3);
    }
}
