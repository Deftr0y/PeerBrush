//! Revalidate actual held journal proofs during the source-root census. The
//! strict journal schema describes self-contained external files, not Store roots.
use super::*;
use crate::file_safety::ExternalCreationRole;

impl Registry {
    fn member(&self, name: &str) -> Option<((u64, u64), [u8; 32])> {
        if let Some(id) = name
            .strip_prefix("external-intent-")
            .and_then(|n| n.strip_suffix(".pbi"))
        {
            let id = *uuid::Uuid::parse_str(id).ok()?.as_bytes();
            let entry = self.entries.iter().find(|entry| entry.frame.id == id)?;
            return Some((entry.file?, entry.sha256));
        }
        if let Some((id, role)) = creation::parse_name(name) {
            let entry = self.entries.iter().find(|entry| entry.frame.id == id)?;
            let at = match role {
                ExternalCreationRole::Temporary => 0,
                ExternalCreationRole::Backup => 1,
            };
            let created = entry.created[at]?;
            return Some((created.file, created.sha256));
        }
        let id = name
            .strip_prefix("external-closed-")?
            .strip_suffix(".pbc")?;
        let id = *uuid::Uuid::parse_str(id).ok()?.as_bytes();
        let entry = self.entries.iter().find(|entry| entry.frame.id == id)?;
        let closed = entry.closed.as_ref()?;
        (entry.closing == Some(closed.file)).then_some((closed.file, closed.sha256))
    }
}
impl SourceSession {
    pub(in crate::workspace::source_runtime) fn publication_member_count(
        &self,
    ) -> Result<usize, String> {
        let state = self
            .runtime
            .inventory
            .try_lock()
            .map_err(|_| "Source inventory is busy")?;
        let inventory = state.as_ref().ok_or("Source inventory unavailable")?;
        inventory
            .publications
            .entries
            .iter()
            .try_fold(0usize, |count, entry| {
                if entry.file.is_none() {
                    return Err("Incomplete publication registration retained".into());
                }
                let closed = match (&entry.closed, entry.closing) {
                    (None, None) => 0,
                    (Some(closed), Some(identity)) if closed.file == identity => 1,
                    _ => return Err("Incomplete publication outcome retained".into()),
                };
                count
                    .checked_add(1 + entry.created.iter().flatten().count() + closed)
                    .ok_or_else(|| "Publication census count overflow".into())
            })
    }
    pub(in crate::workspace::source_runtime) fn check_publication_member(
        &self,
        name: &str,
        identity: (u64, u64),
        check: &dyn Fn() -> Result<(), String>,
    ) -> Result<[u8; 32], String> {
        check()?;
        self.check()?;
        let bytes = self
            .directory
            .as_os_str()
            .len()
            .checked_mul(4)
            .and_then(|n| n.checked_add(4096))
            .ok_or("Journal observation footprint overflow")?;
        let _owner = self
            .resident()
            .reserve_bytes(bytes, bytes)
            .map_err(|e| e.to_string())?;
        let (held, expected) = {
            let state = self
                .runtime
                .inventory
                .try_lock()
                .map_err(|_| "Source inventory is busy")?;
            let inventory = state.as_ref().ok_or("Source inventory unavailable")?;
            let (registered, digest) = inventory
                .publications
                .member(name)
                .ok_or("Unregistered or incomplete publication journal retained")?;
            if registered != identity {
                return Err("Publication journal identity changed".into());
            }
            let held = inventory
                .files
                .iter()
                .find(|held| held.path.file_name().and_then(|n| n.to_str()) == Some(name))
                .ok_or("Publication journal is outside held inventory")?;
            if held.identity != identity || held.bytes > frame::MAX_BYTES as u64 {
                return Err("Publication journal differs from its admitted proof".into());
            }
            (
                HeldFile {
                    path: held.path.clone(),
                    file: held.file.try_clone().map_err(|e| e.to_string())?,
                    identity: held.identity,
                    bytes: held.bytes,
                    modified: held.modified,
                },
                digest,
            )
        };
        held.check()?;
        let mut scratch =
            ScratchBytes::new(&self.resident(), 65536, 65536).map_err(|e| e.to_string())?;
        let mut digest = Context::new(&SHA256);
        let mut at = 0;
        while at < held.bytes {
            check()?;
            self.check()?;
            held.check()?;
            let count = (held.bytes - at).min(scratch.len() as u64) as usize;
            crate::file_safety::owned_rename::read_at(&held.file, at, &mut scratch[..count])?;
            digest.update(&scratch[..count]);
            at += count as u64;
        }
        if digest.finish().as_ref() != expected {
            return Err("Publication journal SHA differs from its registered proof".into());
        }
        held.check()?;
        self.check()?;
        check()?;
        Ok(expected)
    }
}
