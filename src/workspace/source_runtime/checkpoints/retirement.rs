//! Explicit marker disposal. Published backing stores remain retained/charged.
use super::*;
use crate::{
    large_document::{DiskBudget, DiskReservation},
    recovery::backed,
    workspace::source_runtime::HeldFile,
};
use std::{fs::File, path::Path};

/// Minted only after exclusive marker unlink, parent sync and last known close.
/// A census, JSON record or caller-supplied identity cannot construct this receipt.
pub(crate) struct RetiredMarker {
    identity: (u64, u64),
    bytes: u64,
    budget: DiskBudget,
}
impl RetiredMarker {
    pub(crate) fn into_parts(self) -> ((u64, u64), u64, DiskBudget) {
        (self.identity, self.bytes, self.budget)
    }
}

#[cfg(test)]
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum RetirementFault {
    Detached,
    PanicDetached,
    Moved,
    Unlinked,
}

impl SourceSession {
    /// Consume this session so no borrowed cursor can outlive its exclusive
    /// namespace admission. This removes one marker, never shared pixel stores.
    pub(crate) fn discard_checkpoint(
        self,
        id: [u8; 16],
        limits: backed::Limits,
        check: &dyn Fn() -> Result<(), String>,
    ) -> Result<(), String> {
        self.discard_checkpoint_inner(
            id,
            limits,
            check,
            #[cfg(test)]
            None,
        )
    }
    #[cfg(test)]
    pub(crate) fn discard_checkpoint_with_fault(
        self,
        id: [u8; 16],
        limits: backed::Limits,
        check: &dyn Fn() -> Result<(), String>,
        fault: RetirementFault,
    ) -> Result<(), String> {
        self.discard_checkpoint_inner(id, limits, check, Some(fault))
    }
    fn discard_checkpoint_inner(
        mut self,
        id: [u8; 16],
        limits: backed::Limits,
        check: &dyn Fn() -> Result<(), String>,
        #[cfg(test)] fault: Option<RetirementFault>,
    ) -> Result<(), String> {
        // POSIX unlink names cannot prove that a concurrently replaced name
        // still denotes our held marker. Retain all bytes on those platforms.
        if !crate::recovery::NATIVE_DISCARD_AVAILABLE {
            return Err(
                "Native checkpoint discard is unavailable on this platform; files retained".into(),
            );
        }
        check()?;
        let _exclusive = self
            .access
            .take()
            .ok_or("Source cleanup admission unavailable")?
            .exclusive()
            .map_err(|e| e.to_string())?;
        self.check()?;
        let decoded = backed::load(&self, id, limits, check)?;
        let mut marker = decoded.marker;
        self.recheck_checkpoint(&mut marker, check)?;
        // Keep every selected source alive until after registry removal settles.
        let _sources = (decoded.pending, decoded.seals);
        drop(marker);
        let directory = Directory::open(&self.directory)?;
        let path = self
            .directory
            .join(format!("recovery-v3-{}.pbr", uuid::Uuid::from_bytes(id)));
        let expected = {
            let state = self
                .runtime
                .inventory
                .try_lock()
                .map_err(|_| "Source inventory is busy")?;
            let registry = &state
                .as_ref()
                .ok_or("Source inventory unavailable")?
                .checkpoints;
            if registry.ambiguous {
                return Err("Checkpoint registration is ambiguous; roots retained".into());
            }
            let at = registry
                .entries
                .binary_search_by_key(&id, |e| e.marker.snapshot)
                .map_err(|_| "Recovery checkpoint is not registered")?;
            let entry = &registry.entries[at];
            Observation {
                marker: entry.marker,
                sha256: entry.sha256,
            }
        };
        let file = open_for_retirement(&path)?;
        if crate::file_safety::identity(&file)? != expected.marker.identity {
            return Err("Recovery marker owner changed; file retained".into());
        }
        let charge = self
            .disk()
            .retain_file(&path, &file)
            .map_err(|e| e.to_string())?;
        if charge.bytes().map_err(|e| e.to_string())? != expected.marker.bytes {
            return Err("Recovery marker disk admission differs; bytes retained".into());
        }
        check()?;
        self.check()?;
        let (held, root, file_at, root_at) = {
            let mut state = self
                .runtime
                .inventory
                .try_lock()
                .map_err(|_| "Source inventory is busy")?;
            let inventory = state.as_mut().ok_or("Source inventory unavailable")?;
            let registry = &mut inventory.checkpoints;
            let root_at = registry
                .entries
                .binary_search_by_key(&id, |e| e.marker.snapshot)
                .map_err(|_| "Recovery checkpoint registration changed")?;
            let entry = &registry.entries[root_at];
            if registry.ambiguous
                || entry.marker != expected.marker
                || entry.sha256 != expected.sha256
            {
                return Err("Recovery checkpoint registration changed; roots retained".into());
            }
            registry
                .epoch
                .checked_add(2)
                .ok_or("Checkpoint registration epoch exhausted")?;
            let file_at = inventory
                .files
                .iter()
                .position(|f| {
                    f.path == path
                        && f.identity == expected.marker.identity
                        && f.bytes == expected.marker.bytes
                        && f.modified == expected.marker.modified
                })
                .ok_or("Recovery marker differs from its held inventory")?;
            let held = inventory.files.remove(file_at);
            let root = registry.entries.remove(root_at);
            registry.epoch += 1;
            (held, root, file_at, root_at)
        };
        let mut removal = Removal {
            session: &self,
            held: Some(held),
            root: Some(root),
            file_at,
            root_at,
            attempted: false,
        };
        #[cfg(test)]
        match fault {
            Some(RetirementFault::Detached) => return Err("Cancelled after detach".into()),
            Some(RetirementFault::PanicDetached) => panic!("Interrupted after detach"),
            _ => {}
        }
        let verified = (|| -> Result<crate::file_safety::owned_rename::Sealed, String> {
            check()?;
            self.cleanup_check()?;
            removal.held.as_ref().unwrap().check()?;
            // No application callback after this last complete SHA pass.
            let seal = crate::file_safety::owned_rename::Sealed::verify(
                &file,
                expected.marker.bytes,
                expected.sha256,
                &self.resident(),
                &mut || self.cleanup_check(),
            )?;
            directory.check()?;
            removal.held.as_ref().unwrap().check()?;
            Ok(seal)
        })();
        let seal = verified?;
        let retiring = format!("pending-{}", uuid::Uuid::new_v4());
        let retired_path = self.directory.join(&retiring);
        removal.attempted = true;
        // Detach with no replacement and verify the moved owner before unlink.
        // A raced canonical name can only be retained, never deleted as ours.
        let unlinked = crate::file_safety::owned_rename::publish_new(
            directory.path(),
            directory.held(),
            Path::new(
                path.file_name()
                    .ok_or("Invalid recovery marker component")?,
            ),
            Path::new(&retiring),
            &file,
            &seal,
            &mut || self.cleanup_check(),
        )
        .and_then(|_| {
            #[cfg(test)]
            if fault == Some(RetirementFault::Moved) {
                return Err("Interrupted after move".into());
            }
            crate::large_document::unlink_owned_at(&file, directory.held(), Path::new(&retiring))
                .map_err(|e| e.to_string())
        })
        .and_then(|_| {
            if crate::file_safety::link_count(&file)? != 0 {
                return Err("Recovery marker retains a filesystem link; disk debt retained".into());
            }
            Ok(())
        });
        // Both handles must close before a receipt may settle logical-file debt.
        drop(removal.held.take());
        drop(file);
        let settled = unlinked.and_then(|_| {
            #[cfg(test)]
            if fault == Some(RetirementFault::Unlinked) {
                return Err("Interrupted after unlink".into());
            }
            directory.check()?;
            crate::server::sync_parent(&retired_path)?;
            if !matches!(fs::symlink_metadata(&retired_path), Err(e) if e.kind() == std::io::ErrorKind::NotFound) {
                return Err("Recovery marker name remains after unlink; debt retained".into());
            }
            directory.check()?;
            settle(charge, RetiredMarker { identity: expected.marker.identity, bytes: expected.marker.bytes, budget: self.disk() })
        });
        if let Err(error) = settled {
            // No overwriting rollback or guessed refund after an unlink attempt.
            return Err(format!(
                "Recovery disposal is uncertain; backing sources and debt retained. Restart before retrying: {error}"
            ));
        }
        drop(removal.root.take());
        Ok(())
    }
}

fn settle(charge: DiskReservation, receipt: RetiredMarker) -> Result<(), String> {
    charge.settle_checkpoint(receipt).map_err(|e| e.to_string())
}
struct Removal<'a> {
    session: &'a SourceSession,
    held: Option<HeldFile>,
    root: Option<Registered>,
    file_at: usize,
    root_at: usize,
    attempted: bool,
}
impl Drop for Removal<'_> {
    fn drop(&mut self) {
        let Some(root) = self.root.take() else { return };
        if self.attempted {
            drop(self.held.take());
        }
        // Namespace exclusion makes this a bounded memory-only restoration.
        // Keep roots even while unwinding; never guess an unlink refund.
        let mut state = self
            .session
            .runtime
            .inventory
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let inventory = state
            .as_mut()
            .expect("Admitted source inventory remains owned");
        if let Some(held) = self.held.take() {
            inventory.files.insert(self.file_at, held);
        }
        inventory.checkpoints.entries.insert(self.root_at, root);
        inventory.checkpoints.epoch += 1;
        inventory.checkpoints.ambiguous |= self.attempted;
    }
}
fn open_for_retirement(path: &Path) -> Result<File, String> {
    let mut options = fs::OpenOptions::new();
    options.read(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options
            .access_mode(0xC0010000)
            .share_mode(5)
            .custom_flags(0x00200000);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC);
    }
    options
        .open(path)
        .map_err(|_| "Cannot acquire exclusive marker retirement handle; file retained".into())
}
