//! Foreground source admission interrupts background work without dropping its request.
//! This memory-only gate never waits with an Engine/workspace lock held.
use crate::large_document::{Error, Result, SourceAccess, SourceExclusive, SourceRead};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Condvar, Mutex,
};
use std::thread::ThreadId;

#[derive(Default)]
struct State {
    owner: Option<ThreadId>,
    waiters: usize,
}
#[derive(Default)]
pub(super) struct Control {
    state: Mutex<State>,
    wake: Condvar,
    interrupted: AtomicBool,
}
pub(super) struct Collection<'a>(&'a Control);
pub(super) struct ExclusiveOperation<'a> {
    // Release the exclusive permit before waking waiting readers.
    _access: SourceExclusive,
    operation: Collection<'a>,
}
impl ExclusiveOperation<'_> {
    pub(super) fn interrupted(&self) -> bool {
        self.operation.0.interrupted.load(Ordering::Acquire)
    }
}
impl Control {
    pub(super) fn start_exclusive(
        &self,
        access: &Arc<SourceAccess>,
    ) -> Result<ExclusiveOperation<'_>> {
        let operation = self.start()?;
        let exclusive = Arc::new(access.read_owned()?).exclusive()?;
        Ok(ExclusiveOperation {
            _access: exclusive,
            operation,
        })
    }

    pub(super) fn start(&self) -> Result<Collection<'_>> {
        let mut state = self.state.lock().map_err(|_| Error::Interrupted)?;
        if state.owner.is_some() || state.waiters != 0 {
            return Err(Error::Busy);
        }
        self.interrupted.store(false, Ordering::Release);
        state.owner = Some(std::thread::current().id());
        Ok(Collection(self))
    }
    pub(super) fn read(&self, access: &Arc<SourceAccess>) -> Result<SourceRead> {
        let mut state = self.state.lock().map_err(|_| Error::Interrupted)?;
        match access.read_owned() {
            Ok(read) => return Ok(read),
            Err(error)
                if state.owner.is_none() || state.owner == Some(std::thread::current().id()) =>
            {
                return Err(error)
            }
            Err(_) => {}
        }
        state.waiters = state.waiters.checked_add(1).ok_or(Error::Budget)?;
        self.interrupted.store(true, Ordering::Release);
        while state.owner.is_some() {
            state = self.wake.wait(state).map_err(|_| Error::Interrupted)?;
        }
        // The read permit is acquired before releasing the control gate, so a
        // second collection cannot race ahead of an already waiting reader.
        let read = access.read_owned();
        state.waiters -= 1;
        read
    }
    pub(super) fn check(&self) -> std::result::Result<(), String> {
        if self.interrupted.load(Ordering::Acquire) {
            Err("Source collection yielded to foreground work".into())
        } else {
            Ok(())
        }
    }
    #[cfg(test)]
    pub(super) fn interrupt_for_test(&self) {
        self.interrupted.store(true, Ordering::Release);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    #[test]
    fn waiting_reader_interrupts_collection_and_acquires_real_access_before_another_upgrade() {
        let control = Arc::new(Control::default());
        let access = Arc::new(SourceAccess::default());
        let initial = control.read(&access).unwrap();
        assert!(matches!(control.start_exclusive(&access), Err(Error::Busy)));
        drop(initial);
        let operation = control.start_exclusive(&access).unwrap();
        let (ready, started) = mpsc::channel();
        let (send, receive) = mpsc::channel();
        let reader = {
            let control = control.clone();
            let access = access.clone();
            std::thread::spawn(move || {
                ready.send(()).unwrap();
                send.send(control.read(&access)).unwrap();
            })
        };
        started.recv_timeout(Duration::from_secs(2)).unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        while control.check().is_ok() && Instant::now() < deadline {
            std::thread::yield_now();
        }
        assert!(control.check().is_err());
        assert!(matches!(receive.try_recv(), Err(mpsc::TryRecvError::Empty)));
        assert!(control.start().is_err());
        assert!(operation.interrupted());
        drop(operation);
        let admitted = receive
            .recv_timeout(Duration::from_secs(2))
            .unwrap()
            .unwrap();
        assert!(Arc::new(access.read_owned().unwrap()).exclusive().is_err());
        drop(admitted);
        reader.join().unwrap();
        let collection = control.start().unwrap();
        assert!(control.check().is_ok());
        let exclusive = Arc::new(access.read_owned().unwrap()).exclusive().unwrap();
        drop(exclusive);
        drop(collection);
    }

    #[test]
    fn reentrant_checks_and_unrelated_exclusive_work_remain_nonblocking() {
        let control = Control::default();
        let access = Arc::new(SourceAccess::default());
        let read = Arc::new(access.read_owned().unwrap());
        let exclusive = read.exclusive().unwrap();
        assert!(matches!(control.read(&access), Err(Error::Busy)));
        assert!(control.check().is_ok());
        let collection = control.start().unwrap();
        assert!(matches!(control.read(&access), Err(Error::Busy)));
        assert!(control.check().is_ok());
        drop(exclusive);
        drop(collection);
        assert!(control.read(&access).is_ok());
    }
}
impl Drop for Collection<'_> {
    fn drop(&mut self) {
        let mut state = self.0.state.lock().unwrap_or_else(|e| e.into_inner());
        state.owner = None;
        self.0.wake.notify_all();
    }
}
