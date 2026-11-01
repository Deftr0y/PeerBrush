//! Authenticated operation spools. These never become editable source authority.
use super::{Directory, SourceSession};
use crate::large_document::{ByteRole, CompletedBytes, Error, LargeSourceTemporary};
use std::{path::PathBuf, sync::atomic::Ordering};

pub(super) struct Location {
    pub(super) path: PathBuf,
    directory: (u64, u64),
    file: (u64, u64),
}
struct Namespace<'a> {
    session: &'a SourceSession,
    id: [u8; 16],
    directory: Directory,
    file: (u64, u64),
}
impl Namespace<'_> {
    fn io_check(
        &self,
        check: &mut dyn FnMut() -> Result<(), String>,
        failure: &mut Option<String>,
    ) -> crate::large_document::Result<()> {
        if let Err(error) = check().and_then(|_| self.check()) {
            *failure = Some(error);
            return Err(Error::Cancelled);
        }
        Ok(())
    }
    fn check(&self) -> Result<(), String> {
        self.session.check()?;
        self.directory.check()?;
        let path = self.directory.path().join("bytes.pbb");
        let named = crate::file_safety::open_for_identity(&path)
            .map_err(|_| "Operation spool name unavailable; inventory retained")?;
        let metadata = std::fs::symlink_metadata(&path)
            .map_err(|_| "Operation spool disappeared; inventory retained")?;
        if !metadata.is_file()
            || metadata.file_type().is_symlink()
            || crate::file_safety::identity(&named)? != self.file
            || crate::file_safety::link_count(&named)? != 1
        {
            return Err("Operation spool ownership changed; inventory retained".into());
        }
        let state = self
            .session
            .runtime
            .inventory
            .try_lock()
            .map_err(|_| "Source inventory is busy")?;
        let location = state
            .as_ref()
            .and_then(|i| i.staging.get(&self.id))
            .ok_or("Operation spool is outside the admitted inventory")?;
        if location.path != self.directory.path()
            || location.directory != self.directory.identity()
            || location.file != self.file
        {
            return Err("Operation spool registration changed; inventory retained".into());
        }
        Ok(())
    }
    fn retire(self, cleanup: &mut dyn FnMut() -> Result<(), String>) -> Result<(), String> {
        let Self {
            session,
            id,
            directory,
            file,
        } = self;
        let path = directory.path().to_owned();
        let directory_id = directory.identity();
        // The primitive has already proved and retired its sole file owner.
        // Only this exact now-empty directory may release the registry slot.
        if let Err(error) = directory.retire_empty(&mut || {
            cleanup()?;
            session.check()
        }) {
            session.runtime.tainted.store(true, Ordering::Release);
            return Err(error);
        }
        session.check()?;
        let mut state = session
            .runtime
            .inventory
            .try_lock()
            .map_err(|_| "Source inventory is busy")?;
        let inventory = state.as_mut().ok_or("Source inventory unavailable")?;
        let location = inventory
            .staging
            .get(&id)
            .ok_or("Retired operation registration disappeared")?;
        if location.path != path || location.directory != directory_id || location.file != file {
            session.runtime.tainted.store(true, Ordering::Release);
            return Err("Retired operation registration changed".into());
        }
        inventory.staging.remove(&id);
        inventory.directories = inventory
            .directories
            .checked_sub(1)
            .ok_or("Operation directory inventory count changed")?;
        Ok(())
    }
}

/// The borrowed session retains both profile/instance and WorkspaceFileOperation
/// admission through every read, write, publication and explicit cleanup.
pub(crate) struct SessionTemporary<'a> {
    temporary: LargeSourceTemporary,
    namespace: Namespace<'a>,
}
pub(crate) struct SessionBytes<'a> {
    bytes: CompletedBytes,
    namespace: Namespace<'a>,
}
impl SourceSession {
    pub(crate) fn byte_temporary(
        &self,
        length: u64,
        role: ByteRole,
    ) -> Result<SessionTemporary<'_>, String> {
        self.check()?;
        self.reserve_directory()?;
        let id = *uuid::Uuid::new_v4().as_bytes();
        let path = self
            .directory
            .join(format!("bytes-{}", uuid::Uuid::from_bytes(id)));
        // A failed constructor deliberately leaves its quota/directory slot
        // retained. Startup inventories known files; unknown contents fail closed.
        let temporary =
            LargeSourceTemporary::create(&path, length, role, self.resident(), self.disk())
                .map_err(|e| e.to_string())?;
        let directory = Directory::open(&path)?;
        let file = crate::file_safety::open_for_identity(&path.join("bytes.pbb"))
            .map_err(|_| "New operation spool unavailable; inventory retained")?;
        let file_id = crate::file_safety::identity(&file)?;
        if temporary.filesystem_identity() != (directory.identity(), file_id)
            || crate::file_safety::link_count(&file)? != 1
        {
            return Err("New operation spool has an alias; inventory retained".into());
        }
        // Do not retain another file handle: verified retirement must close the
        // last physical owner before its persistent file charge can be released.
        drop(file);
        let mut state = self
            .runtime
            .inventory
            .try_lock()
            .map_err(|_| "Source inventory is busy")?;
        let inventory = state.as_mut().ok_or("Source inventory unavailable")?;
        if inventory.staging.contains_key(&id) {
            return Err("Duplicate operation spool identity retained".into());
        }
        inventory.staging.insert(
            id,
            Location {
                path,
                directory: directory.identity(),
                file: file_id,
            },
        );
        drop(state);
        let namespace = Namespace {
            session: self,
            id,
            directory,
            file: file_id,
        };
        namespace.check()?;
        Ok(SessionTemporary {
            temporary,
            namespace,
        })
    }
}
impl<'a> SessionTemporary<'a> {
    /// Caller checks must also be supplied to writer/append; this reference
    /// grants no independent filesystem or editable-document authority.
    pub(crate) fn temporary(&mut self) -> &mut LargeSourceTemporary {
        &mut self.temporary
    }
    pub(crate) fn check(&self) -> Result<(), String> {
        self.namespace.check()
    }
    pub(crate) fn finish(
        self,
        binding: [u8; 32],
        check: &mut dyn FnMut() -> Result<(), String>,
    ) -> Result<SessionBytes<'a>, String> {
        let Self {
            temporary,
            namespace,
        } = self;
        let mut failure = None;
        let bytes = temporary
            .finish(binding, &mut || namespace.io_check(check, &mut failure))
            .map_err(|e| failure.unwrap_or_else(|| e.to_string()))?;
        namespace.check()?;
        Ok(SessionBytes { bytes, namespace })
    }
    pub(crate) fn abort(
        self,
        cleanup: &mut dyn FnMut() -> Result<(), String>,
    ) -> Result<(), String> {
        let Self {
            temporary,
            namespace,
        } = self;
        let mut failure = None;
        temporary
            .abort(&mut || namespace.io_check(cleanup, &mut failure))
            .map_err(|e| failure.unwrap_or_else(|| e.to_string()))?;
        namespace.retire(cleanup)
    }
}
impl SessionBytes<'_> {
    /// Retain this guard until the last cursor/read and final publication check.
    pub(crate) fn bytes(&self) -> &CompletedBytes {
        &self.bytes
    }
    pub(crate) fn check(&self) -> Result<(), String> {
        self.namespace.check()
    }
    pub(crate) fn retire(
        self,
        cleanup: &mut dyn FnMut() -> Result<(), String>,
    ) -> Result<(), String> {
        let Self { bytes, namespace } = self;
        let mut failure = None;
        bytes
            .retire(&mut || namespace.io_check(cleanup, &mut failure))
            .map_err(|e| failure.unwrap_or_else(|| e.to_string()))?;
        namespace.retire(cleanup)
    }
}
