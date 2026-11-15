//! Real child ownership outlives pipe cancellation, failed stops and results.
//! The child receives only metadata and pipe replies, never source sessions.
use super::{parent, resources};
use crate::{
    code::{guard, native_source::NativeSource, Job},
    engine::{native::NativeDocument, view::DocumentView},
    psd::memory36::{Admitted, MetadataCharge},
    server::Shared,
    worker_pipe::Pipe,
};
use serde_json::Value;
use std::{
    process::{Child, ChildStdin, ChildStdout, Command, ExitStatus, Stdio},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc, Condvar, Mutex, OnceLock,
    },
    thread,
    time::{Duration, Instant},
};
type Result<T> = std::result::Result<T, String>;
const FAILED: &str = "Code child failed; current work preserved";
const RETAINED: &str = "Code child exit is unconfirmed; its memory remains reserved";
const STACK_BYTES: usize = 2 * 1024 * 1024;
const OWNED_BYTES: usize = resources::MEMORY + STACK_BYTES + 65536;

struct Owned {
    child: Child,
    limits: Option<resources::Parent>,
    _charge: Arc<MetadataCharge>,
}
impl Owned {
    fn kill(&mut self) {
        if self.child.kill().is_err() {
            #[cfg(windows)]
            if let Some(limits) = &self.limits {
                limits.terminate();
            }
            #[cfg(unix)]
            {
                let _ = self.child.kill();
            }
        }
    }
}
type Slot = Arc<Mutex<Owned>>;
static CHILDREN: Mutex<[Option<Slot>; 2]> = Mutex::new([None, None]);
static WAKE: Condvar = Condvar::new();
static REAPER: OnceLock<Result<()>> = OnceLock::new();
static REAPER_LIVE: AtomicBool = AtomicBool::new(false);

fn ensure_reaper() -> Result<()> {
    REAPER
        .get_or_init(|| {
            let (tx, rx) = mpsc::sync_channel(1);
            thread::Builder::new()
                .name("peerbrush-code-reaper".into())
                .spawn(move || {
                    struct Live;
                    impl Drop for Live {
                        fn drop(&mut self) {
                            REAPER_LIVE.store(false, Ordering::SeqCst);
                        }
                    }
                    let _live = Live;
                    REAPER_LIVE.store(true, Ordering::SeqCst);
                    let _ = tx.send(());
                    loop {
                        let retired = {
                            let mut slots = CHILDREN.lock().unwrap_or_else(|e| e.into_inner());
                            let mut retired: [Option<Slot>; 2] = [None, None];
                            for index in 0..slots.len() {
                                // A controller/pump scope still owns another actual Arc
                                // until its threads and success/failure checks finish.
                                let stopped = slots[index].as_ref().is_some_and(|slot| {
                                    if Arc::strong_count(slot) != 1 {
                                        return false;
                                    }
                                    let mut owned = slot.lock().unwrap_or_else(|e| e.into_inner());
                                    match owned.child.try_wait() {
                                        Ok(Some(_)) => true,
                                        Ok(None) => {
                                            owned.kill();
                                            false
                                        }
                                        Err(_) => false,
                                    }
                                });
                                if stopped {
                                    retired[index] = slots[index].take();
                                }
                            }
                            retired
                        };
                        // Closing handles and releasing budget owners happens off the
                        // registry mutex, and only after an actual Child exit result.
                        drop(retired);
                        let slots = CHILDREN.lock().unwrap_or_else(|e| e.into_inner());
                        if slots.iter().any(Option::is_some) {
                            drop(
                                WAKE.wait_timeout(slots, Duration::from_millis(50))
                                    .unwrap_or_else(|e| e.into_inner()),
                            );
                        } else {
                            drop(WAKE.wait(slots).unwrap_or_else(|e| e.into_inner()));
                        }
                    }
                })
                .map_err(|_| "Code child reaper could not start")?;
            rx.recv_timeout(Duration::from_secs(1))
                .map_err(|_| "Code child reaper did not become ready")?;
            Ok(())
        })
        .clone()?;
    if !REAPER_LIVE.load(Ordering::SeqCst) {
        return Err("Code child reaper is unavailable".into());
    }
    Ok(())
}

struct Guard {
    owner: Slot,
    stop: Arc<AtomicBool>,
    attempted_stop: bool,
}
impl Guard {
    fn launch(mut command: Command, charge: Arc<MetadataCharge>) -> Result<Self> {
        ensure_reaper()?;
        let stop = Arc::new(AtomicBool::new(false));
        let mut slots = CHILDREN.lock().unwrap_or_else(|e| e.into_inner());
        let free = slots
            .iter()
            .position(Option::is_none)
            .ok_or("Two code child owners remain active; wait for confirmed exit")?;
        let child = command.spawn().map_err(|_| FAILED)?;
        let owner = Arc::new(Mutex::new(Owned {
            child,
            limits: None,
            _charge: charge,
        }));
        slots[free] = Some(owner.clone());
        drop(slots);
        WAKE.notify_one();
        Ok(Self {
            owner,
            stop,
            attempted_stop: false,
        })
    }
    fn pipes(&self) -> Result<(Pipe<ChildStdout>, Pipe<ChildStdin>)> {
        let mut owned = self.owner.lock().unwrap_or_else(|e| e.into_inner());
        owned.limits = Some(resources::Parent::new(&owned.child)?);
        let stdin = owned.child.stdin.take().ok_or(FAILED)?;
        let stdout = owned.child.stdout.take().ok_or(FAILED)?;
        Ok((
            Pipe::reader(stdout, self.stop.clone()).map_err(|_| FAILED)?,
            Pipe::writer(stdin, self.stop.clone()).map_err(|_| FAILED)?,
        ))
    }
    fn observe(&self) -> Result<Option<ExitStatus>> {
        self.owner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .child
            .try_wait()
            .map_err(|_| FAILED.into())
    }
    fn stop(&mut self) -> Result<()> {
        self.attempted_stop = true;
        self.stop.store(true, Ordering::SeqCst);
        let until = Instant::now() + Duration::from_secs(3);
        {
            let mut owned = self.owner.lock().unwrap_or_else(|e| e.into_inner());
            match owned.child.try_wait() {
                Ok(Some(_)) => return Ok(()),
                Ok(None) => (),
                Err(_) => return Err(RETAINED.into()),
            }
            owned.kill();
        }
        loop {
            match self.observe() {
                Ok(Some(_)) => return Ok(()),
                Ok(None) => (),
                Err(_) => return Err(RETAINED.into()),
            }
            if Instant::now() >= until {
                return Err(RETAINED.into());
            }
            thread::sleep(Duration::from_millis(10));
        }
    }
}
impl Drop for Guard {
    fn drop(&mut self) {
        if !self.attempted_stop {
            let _ = self.stop();
        }
        if matches!(self.observe(), Ok(Some(_))) {
            let retired = {
                let mut slots = CHILDREN.lock().unwrap_or_else(|e| e.into_inner());
                slots
                    .iter_mut()
                    .find(|slot| {
                        slot.as_ref()
                            .is_some_and(|owner| Arc::ptr_eq(owner, &self.owner))
                    })
                    .and_then(Option::take)
            };
            // A completed child should not withhold its large ticket while the
            // next native preparation starts. The enclosing scope still holds
            // the stack/header charge until all pipe threads have joined.
            drop(retired);
        }
        // The registry owns the same Child, limits and charge after this Arc
        // disappears whenever actual exit is still unconfirmed.
        WAKE.notify_one();
    }
}

fn supervise<T: Send>(
    command: Command,
    charge: Arc<MetadataCharge>,
    check: &(dyn Fn() -> Result<()> + Sync),
    exchange: impl FnOnce(Pipe<ChildStdout>, Pipe<ChildStdin>) -> Result<T> + Send,
) -> Result<T> {
    check()?;
    // Also retain the explicit pump-stack/header admission outside scope's
    // implicit joins when its controller is unwinding.
    let _scope_charge = charge.clone();
    thread::scope(|scope| {
        // This guard must unwind before scope's implicit joins, so a watchdog
        // panic also interrupts a pipe pump waiting on a silent child.
        let mut guard = Guard::launch(command, charge)?;
        let (reader, writer) = guard.pipes()?;
        let (tx, rx) = mpsc::sync_channel(1);
        let pump = thread::Builder::new()
            .name("peerbrush-code-pipe".into())
            .stack_size(STACK_BYTES)
            .spawn_scoped(scope, move || {
                let _ = tx.send(exchange(reader, writer));
            })
            .map_err(|_| "Code pipe worker could not start")?;
        let mut packet = None;
        let outcome: Result<()> = (|| loop {
            check()?;
            match rx.try_recv() {
                Ok(result) => {
                    let error = result.as_ref().err().cloned();
                    packet = Some(result);
                    if let Some(error) = error {
                        return Err(error);
                    }
                }
                Err(mpsc::TryRecvError::Empty) => (),
                Err(mpsc::TryRecvError::Disconnected) if packet.is_some() => (),
                Err(_) => return Err(FAILED.into()),
            }
            if let Some(status) = guard.observe()? {
                if !status.success() {
                    return Err(FAILED.into());
                }
                if packet.is_some() {
                    return Ok(());
                }
            }
            thread::sleep(Duration::from_millis(10));
        })();
        // Nonblocking pipes observe this flag independently of process death.
        // Source/callback owners stay in the joined scope until its pump ends.
        let cleanup = guard.stop();
        let joined = pump.join().map_err(|_| "Code pipe worker panicked");
        if packet.is_none() {
            packet = rx.try_recv().ok();
        }
        cleanup?;
        joined?;
        outcome?;
        packet.ok_or(FAILED)?
    })
}

fn command() -> Result<Command> {
    let executable = std::env::current_exe()
        .and_then(|p| p.canonicalize())
        .map_err(|_| FAILED)?;
    let mut command = Command::new(&executable);
    command
        .current_dir(executable.parent().ok_or(FAILED)?)
        .arg("--code-worker")
        .env_clear()
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000);
        for key in ["SystemRoot", "WINDIR"] {
            if let Some(value) = std::env::var_os(key) {
                command.env(key, value);
            }
        }
    }
    Ok(command)
}

pub(in crate::code) fn commands(
    shared: &Shared,
    job: &Job,
    document: &NativeDocument,
    script: &str,
    started: Instant,
    check: &(dyn Fn() -> Result<()> + Sync),
) -> Result<Admitted<Vec<Value>>> {
    resources::Parent::supported()?;
    if script.trim().is_empty() || script.len() > super::protocol::SCRIPT_LIMIT {
        return Err("Script must contain 1–65536 UTF-8 bytes".into());
    }
    let active = || {
        check()?;
        if job.cancel.load(Ordering::Relaxed) || started.elapsed() >= Duration::from_secs(30) {
            return Err(
                "Code canceled or preparation time limit exceeded; no edits committed".into(),
            );
        }
        // Strict source/session checks remain in capture and every exchange.
        // The watchdog also observes task takeover while the child emits no I/O.
        if let Ok(mut engine) = shared.try_lock() {
            guard(&mut engine, job, DocumentView::from(document))?;
        }
        Ok(())
    };
    let mut source = NativeSource::capture(shared, job, document, &active)?;
    let charge = MetadataCharge::reserve(source.budget(), OWNED_BYTES, &active)?;
    let command = command()?;
    let received = supervise(command, charge, &active, |reader, writer| {
        parent::exchange(
            shared,
            job,
            &mut source,
            script,
            *uuid::Uuid::new_v4().as_bytes(),
            reader,
            writer,
            started,
            &active,
        )
    })?;
    source.check(shared, job, &active)?;
    Ok(received.into_commands())
}

#[cfg(test)]
mod tests;
