use std::sync::{
    atomic::{AtomicIsize, Ordering},
    Arc,
};

use super::{Error, Result};

/// A nonblocking operation fence, so callbacks never run under a store mutex.
#[derive(Default)]
pub(crate) struct Access(AtomicIsize);

pub(super) struct Read<'a>(&'a AtomicIsize);
pub(super) struct Write<'a>(&'a AtomicIsize);
pub(crate) struct OwnedRead(Arc<Access>);
pub(crate) struct Exclusive(OwnedRead);

impl Access {
    pub(crate) fn read_owned(self: &Arc<Self>) -> Result<OwnedRead> {
        let read = self.read()?;
        // Transfer the same counted admission into its owning guard.
        std::mem::forget(read);
        Ok(OwnedRead(self.clone()))
    }
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
impl OwnedRead {
    pub(crate) fn exclusive(self: Arc<Self>) -> Result<Exclusive> {
        let owned = Arc::try_unwrap(self).map_err(|_| Error::Busy)?;
        owned
            .0
             .0
            .compare_exchange(1, -1, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| Error::Busy)?;
        Ok(Exclusive(owned))
    }
}
impl Drop for OwnedRead {
    fn drop(&mut self) {
        self.0 .0.fetch_sub(1, Ordering::AcqRel);
    }
}
impl Drop for Exclusive {
    fn drop(&mut self) {
        self.0 .0 .0.store(1, Ordering::Release);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exclusive_upgrade_requires_every_reader_and_child_to_leave() {
        let access = Arc::new(Access::default());
        let reader = access.read_owned().unwrap();
        let other = access.read_owned().unwrap();
        assert!(matches!(Arc::new(reader).exclusive(), Err(Error::Busy)));
        drop(other);
        let reader = Arc::new(access.read_owned().unwrap());
        let child = reader.clone();
        assert!(matches!(reader.exclusive(), Err(Error::Busy)));
        assert!(matches!(access.write(), Err(Error::Busy)));
        drop(child);
        let exclusive = Arc::new(access.read_owned().unwrap()).exclusive().unwrap();
        assert!(matches!(access.read_owned(), Err(Error::Busy)));
        assert!(matches!(access.read(), Err(Error::Busy)));
        assert!(matches!(access.write(), Err(Error::Busy)));
        drop(exclusive);
        assert!(access.write().is_ok());
        assert!(access.read_owned().is_ok());
    }
}
