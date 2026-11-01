//! Held native recovery marker I/O. A marker's hash is integrity, not authentication.
use super::{Directory, HeldFile, SourceSession};
use crate::file_safety::owned_rename;
use std::{
    fs::File,
    io::{self, Read, Seek, SeekFrom, Write},
    path::PathBuf,
};

const MAX_MARKER: u64 = 128 * 1024 * 1024;

pub(crate) struct NativeMarkerIds {
    entries: Vec<[u8; 16]>,
    _charge: crate::large_document::Reservation,
}
impl NativeMarkerIds {
    /// Discovery is not framing, source or editable-document qualification.
    pub(crate) fn iter(&self) -> impl Iterator<Item = [u8; 16]> + '_ {
        self.entries.iter().copied()
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) struct MarkerIdentity {
    pub(super) snapshot: [u8; 16],
    pub(super) identity: (u64, u64),
    pub(super) bytes: u64,
    pub(super) modified: std::time::SystemTime,
}
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
    pub(super) fn check_session(&self, session: &SourceSession) -> Result<(), String> {
        if !std::ptr::eq(self.session, session) {
            return Err("Checkpoint marker belongs to another operation".into());
        }
        self.check()
    }
    pub(super) fn verify_digest(
        &mut self,
        expected: [u8; 32],
        check: &dyn Fn() -> Result<(), String>,
    ) -> Result<MarkerIdentity, String> {
        check()?;
        self.check()?;
        self.file
            .seek(SeekFrom::Start(0))
            .map_err(|_| "Cannot inspect checkpoint marker")?;
        let mut scratch = crate::large_document::ScratchBytes::new(
            &self.session.resident(),
            64 * 1024,
            64 * 1024,
        )
        .map_err(|e| e.to_string())?;
        let mut hash = ring::digest::Context::new(&ring::digest::SHA256);
        let mut remaining = self.bytes;
        while remaining != 0 {
            check()?;
            self.check()?;
            let count = remaining.min(scratch.len() as u64) as usize;
            self.file
                .read_exact(&mut scratch[..count])
                .map_err(|_| "Checkpoint marker is truncated")?;
            hash.update(&scratch[..count]);
            remaining -= count as u64;
        }
        let mut extra = [0];
        if self
            .file
            .read(&mut extra)
            .map_err(|_| "Checkpoint final read failed")?
            != 0
            || hash.finish().as_ref() != expected
        {
            return Err("Checkpoint complete SHA or EOF differs; roots retained".into());
        }
        check()?;
        self.check()?;
        Ok(MarkerIdentity {
            snapshot: self.snapshot,
            identity: self.identity,
            bytes: self.bytes,
            modified: self.modified,
        })
    }

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
    pub(crate) fn recovery_marker_ids(&self) -> Result<NativeMarkerIds, String> {
        self.check()?;
        let maximum = self.runtime.limits.max_files;
        let bytes = maximum
            .checked_mul(2 * std::mem::size_of::<[u8; 16]>())
            .and_then(|n| n.checked_add(128))
            .ok_or("Recovery discovery footprint overflow")?;
        let charge = self
            .resident()
            .reserve_bytes(bytes, 64 * 1024 * 1024)
            .map_err(|e| e.to_string())?;
        let mut entries = Vec::new();
        entries
            .try_reserve_exact(maximum)
            .map_err(|_| "Recovery discovery allocation refused")?;
        if entries.capacity() > maximum.saturating_mul(2) {
            return Err("Recovery discovery exceeds admitted capacity".into());
        }
        let state = self
            .runtime
            .inventory
            .try_lock()
            .map_err(|_| "Source inventory is busy")?;
        let inventory = state.as_ref().ok_or("Source inventory unavailable")?;
        for held in &inventory.files {
            let Some(name) = held.path.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            let Some(id) = name
                .strip_prefix("recovery-v3-")
                .and_then(|n| n.strip_suffix(".pbr"))
            else {
                continue;
            };
            let id = uuid::Uuid::parse_str(id)
                .map_err(|_| "Invalid native recovery identity retained")?;
            entries.push(*id.as_bytes());
        }
        drop(state);
        entries.sort_unstable();
        if entries.windows(2).any(|w| w[0] == w[1]) {
            return Err("Ambiguous native recovery identities retained".into());
        }
        self.check()?;
        Ok(NativeMarkerIds {
            entries,
            _charge: charge,
        })
    }
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

#[cfg(test)]
impl SourceSession {
    /// Fault injection through the actual owned writer, never a production API.
    pub(crate) fn corrupt_checkpoint_for_test(
        &self,
        id: [u8; 16],
        offset: u64,
    ) -> Result<(), String> {
        self.check()?;
        let path = self
            .directory
            .join(format!("recovery-v3-{}.pbr", uuid::Uuid::from_bytes(id)));
        let state = self.runtime.inventory.lock().unwrap();
        let held = state
            .as_ref()
            .unwrap()
            .files
            .iter()
            .find(|held| held.path == path)
            .ok_or("Missing test marker")?;
        if offset >= held.bytes {
            return Err("Test offset exceeds marker".into());
        }
        let mut file = &held.file;
        file.seek(SeekFrom::Start(offset))
            .map_err(|e| e.to_string())?;
        let mut byte = [0];
        file.read_exact(&mut byte).map_err(|e| e.to_string())?;
        byte[0] ^= 0x55;
        file.seek(SeekFrom::Start(offset))
            .map_err(|e| e.to_string())?;
        file.write_all(&byte).map_err(|e| e.to_string())?;
        file.set_times(std::fs::FileTimes::new().set_modified(held.modified))
            .map_err(|e| e.to_string())?;
        Ok(())
    }
}
