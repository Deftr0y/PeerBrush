use std::sync::atomic::{AtomicIsize, Ordering};

use super::{Error, Result};

/// A nonblocking operation fence, so callbacks never run under a store mutex.
#[derive(Default)]
pub(super) struct Access(AtomicIsize);

pub(super) struct Read<'a>(&'a AtomicIsize);
pub(super) struct Write<'a>(&'a AtomicIsize);

impl Access {
    pub(super) fn read(&self) -> Result<Read<'_>> {
        self.0
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |readers| {
                (readers >= 0).then(|| readers.checked_add(1)).flatten()
            })
            .map_err(|_| Error::Busy)?;
        Ok(Read(&self.0))
    }

    pub(super) fn write(&self) -> Result<Write<'_>> {
        self.0
            .compare_exchange(0, -1, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| Error::Busy)?;
        Ok(Write(&self.0))
    }
}

impl Drop for Read<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

impl Drop for Write<'_> {
    fn drop(&mut self) {
        self.0.store(0, Ordering::Release);
    }
}
