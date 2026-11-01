//! Held native recovery marker I/O. A marker's hash is integrity, not authentication.
use super::{Directory, HeldFile, SourceSession};
use crate::file_safety::owned_rename;
use std::{
    fs::File,
    io::{self, Read, Seek, SeekFrom, Write},
    path::PathBuf,
};

const MAX_MARKER: u64 = 128 * 1024 * 1024;

pub(crate) struct NativeMarker<'a> {
    session: &'a SourceSession,
    snapshot: [u8; 16],
    path: PathBuf,
    file: File,
    identity: (u64, u64),
    bytes: u64,
    modified: std::time::SystemTime,
}
impl NativeMarker<'_> {
    pub(crate) fn snapshot(&self) -> [u8; 16] {
        self.snapshot
    }
    pub(crate) fn len(&self) -> u64 {
        self.bytes
    }
    pub(crate) fn check(&self) -> Result<(), String> {
        self.session.check()?;
        let named = crate::file_safety::open_native_identity(&self.path)
            .map_err(|_| "Native recovery marker disappeared")?;
        let actual = self
            .file
            .metadata()
            .map_err(|_| "Native recovery marker unavailable")?;
        if crate::file_safety::identity(&named)? != self.identity
            || crate::file_safety::identity(&self.file)? != self.identity
            || crate::file_safety::link_count(&self.file)? != 1
            || actual.len() != self.bytes
            || actual.modified().ok() != Some(self.modified)
        {
            return Err("Native recovery marker changed; bytes retained".into());
        }
        Ok(())
    }
}
impl Read for NativeMarker<'_> {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        self.check().map_err(io::Error::other)?;
        let count = bytes.len().min(64 * 1024);
        self.file.read(&mut bytes[..count])
    }
}
impl Seek for NativeMarker<'_> {
    fn seek(&mut self, from: SeekFrom) -> io::Result<u64> {
        self.check().map_err(io::Error::other)?;
        self.file.seek(from)
    }
}

struct Writer<'a> {
    file: &'a mut File,
    remaining: u64,
    session: &'a SourceSession,
    check: &'a dyn Fn() -> Result<(), String>,
}
impl Write for Writer<'_> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        (self.check)()
            .and_then(|_| self.session.check())
            .map_err(io::Error::other)?;
        if bytes.len() as u64 > self.remaining {
            return Err(io::Error::other(
                "Native recovery marker exceeds admitted length",
            ));
        }
        let written = self.file.write(&bytes[..bytes.len().min(64 * 1024)])?;
        self.remaining -= written as u64;
        Ok(written)
    }
    fn flush(&mut self) -> io::Result<()> {
        (self.check)()
            .and_then(|_| self.session.check())
            .map_err(io::Error::other)
    }
}
impl SourceSession {
    /// The producer emits a completely framed recovery-domain record. Final
    /// ownership/revision checks occur after durable SHA readback and immediately
    /// before exclusive publication. No path is accepted from a serialized input.
    pub(crate) fn publish_recovery_marker(
        &self,
        snapshot: [u8; 16],
        bytes: u64,
        sha256: [u8; 32],
        producer: &mut dyn FnMut(&mut dyn Write) -> Result<(), String>,
        check: &dyn Fn() -> Result<(), String>,
    ) -> Result<(), String> {
        if snapshot == [0; 16] || bytes == 0 || bytes > MAX_MARKER {
            return Err("Invalid native recovery marker admission".into());
        }
        check()?;
        self.check()?;
        let directory = Directory::open(&self.directory)?;
        let charge = self.disk().reserve(bytes).map_err(|e| e.to_string())?;
        let uuid = uuid::Uuid::from_bytes(snapshot);
        let from = format!("pending-{uuid}");
        let to = format!("recovery-v3-{uuid}.pbr");
        let mut file = owned_rename::staging_options()
            .open(self.directory.join(&from))
            .map_err(|_| "Cannot create exclusive native recovery marker")?;
        charge.bind(&file).map_err(|e| e.to_string())?;
        let mut writer = Writer {
            file: &mut file,
            remaining: bytes,
            session: self,
            check,
        };
        producer(&mut writer)?;
        if writer.remaining != 0 {
            return Err("Native recovery marker producer ended before its admitted length".into());
        }
        drop(writer);
        let proof =
            owned_rename::Sealed::verify(&file, bytes, sha256, &self.resident(), &mut || {
                check()?;
                self.check()
            })?;
        owned_rename::publish_new(
            directory.path(),
            directory.held(),
            std::path::Path::new(&from),
            std::path::Path::new(&to),
            &file,
            &proof,
            &mut || {
                check()?;
                self.check()
            },
        )?;
        let held = HeldFile::new(self.directory.join(to), file)?;
        check()?;
        self.check()?;
        let mut state = self
            .runtime
            .inventory
            .try_lock()
            .map_err(|_| "Source inventory is busy")?;
        let inventory = state.as_mut().ok_or("Source inventory unavailable")?;
        if inventory.files.len() >= self.runtime.limits.max_files {
            return Err(
                "Native recovery marker inventory capacity exceeded; bytes retained".into(),
            );
        }
        inventory
            .files
            .try_reserve(1)
            .map_err(|_| "Native recovery marker inventory allocation refused")?;
        inventory.files.push(held);
        drop(state);
        self.check()
    }

    pub(crate) fn open_recovery_marker(
        &self,
        snapshot: [u8; 16],
    ) -> Result<NativeMarker<'_>, String> {
        self.check()?;
        let path = self.directory.join(format!(
            "recovery-v3-{}.pbr",
            uuid::Uuid::from_bytes(snapshot)
        ));
        let (identity, bytes, modified) = {
            let state = self
                .runtime
                .inventory
                .try_lock()
                .map_err(|_| "Source inventory is busy")?;
            let held = state
                .as_ref()
                .and_then(|i| i.files.iter().find(|f| f.path == path))
                .ok_or("Native recovery marker is outside the authenticated inventory")?;
            (held.identity, held.bytes, held.modified)
        };
        if bytes == 0 || bytes > MAX_MARKER {
            return Err("Native recovery marker size refused".into());
        }
        let file = super::open_read(&path)?;
        let marker = NativeMarker {
            session: self,
            snapshot,
            path,
            file,
            identity,
            bytes,
            modified,
        };
        marker.check()?;
        Ok(marker)
    }
}
