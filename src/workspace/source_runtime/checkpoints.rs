//! Persistent marker roots. Observation never grants deletion or adoption.
use super::{
    recovery_marker::{MarkerIdentity, NativeMarker},
    Directory, SourceSession,
};
use crate::{
    large_document::{Reservation, Snapshot},
    psd::memory36::MetadataCharge,
    recovery::backed::CheckpointProof,
};
use std::{fs, sync::Arc};
#[path = "checkpoints/retirement.rs"]
mod retirement;
pub(crate) use retirement::RetiredMarker;
#[cfg(test)]
pub(crate) use retirement::RetirementFault;

pub(super) struct Registry {
    entries: Vec<Registered>,
    maximum: usize,
    epoch: u64,
    ambiguous: bool,
}
struct Registered {
    marker: MarkerIdentity,
    sha256: [u8; 32],
    sources: Vec<Snapshot>,
    _owner: Arc<MetadataCharge>,
}
#[derive(Clone, Copy)]
struct Observation {
    marker: MarkerIdentity,
    sha256: [u8; 32],
}
pub(crate) struct CheckpointCensus {
    registered: usize,
}
impl CheckpointCensus {
    /// A diagnostic count, never an absence-of-live-roots or retirement proof.
    pub(crate) fn registered_count(&self) -> usize {
        self.registered
    }
}
impl Registry {
    pub(super) fn retain_uncertain_publication(&mut self) {
        self.ambiguous = true;
    }
    pub(super) fn ambiguous(&self) -> bool {
        self.ambiguous
    }
    pub(super) fn new(maximum: usize) -> Result<Self, String> {
        let mut entries = Vec::new();
        // The encompassing inventory reserves >=8192 bytes per entry before
        // this allocation. Snapshot rosters carry their separate proof charge.
        entries
            .try_reserve_exact(maximum)
            .map_err(|_| "Checkpoint registry allocation refused")?;
        if entries
            .capacity()
            .checked_mul(std::mem::size_of::<Registered>())
            .is_none_or(|n| n > maximum.saturating_mul(8192))
        {
            return Err("Checkpoint registry capacity exceeds admission".into());
        }
        Ok(Self {
            entries,
            maximum,
            epoch: 0,
            ambiguous: false,
        })
    }
    fn prepare(
        &mut self,
        marker: MarkerIdentity,
        parts: &crate::recovery::backed::CheckpointParts,
    ) -> Result<Option<(usize, u64)>, String> {
        match self
            .entries
            .binary_search_by_key(&marker.snapshot, |entry| entry.marker.snapshot)
        {
            Ok(at) => {
                let previous = &self.entries[at];
                if previous.marker != marker
                    || previous.sha256 != parts.sha256
                    || previous
                        .sources
                        .iter()
                        .map(|s| (s.store().identity(), s.reference()))
                        .ne(parts
                            .sources
                            .iter()
                            .map(|s| (s.store().identity(), s.reference())))
                {
                    self.ambiguous = true;
                    return Err("Conflicting checkpoint registration retained".into());
                }
                Ok(None)
            }
            Err(at) => {
                if self.entries.len() == self.maximum
                    || self.entries.len() == self.entries.capacity()
                {
                    return Err(
                        "Checkpoint registration capacity exhausted; marker retained".into(),
                    );
                }
                let epoch = self
                    .epoch
                    .checked_add(1)
                    .ok_or("Checkpoint registration epoch exhausted")?;
                Ok(Some((at, epoch)))
            }
        }
    }
    fn insert_prepared(
        &mut self,
        at: usize,
        epoch: u64,
        marker: MarkerIdentity,
        parts: crate::recovery::backed::CheckpointParts,
    ) {
        self.entries.insert(
            at,
            Registered {
                marker,
                sha256: parts.sha256,
                sources: parts.sources,
                _owner: parts.owner,
            },
        );
        self.epoch = epoch;
    }
}
struct Observations {
    entries: Vec<Observation>,
    _charge: Reservation,
    epoch: u64,
}
#[derive(PartialEq, Eq, PartialOrd, Ord)]
struct Member {
    name: [u8; 80],
    kind: u8,
    identity: (u64, u64),
    child: (u64, u64),
    journal_sha256: [u8; 32],
}
struct Members {
    entries: Vec<Member>,
    _charge: Reservation,
}
impl SourceSession {
    /// Reuse a fully registered source selection, not an inferred file head.
    pub(super) fn retained_checkpoint_source(
        &self,
        store: &crate::large_document::Store,
        needed: &mut [([u8; 32], bool)],
    ) -> Result<Option<Snapshot>, String> {
        self.check()?;
        let (epoch, count) = {
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
                return Err("Checkpoint registration is uncertain; sources retained".into());
            }
            (registry.epoch, registry.entries.len())
        };
        for at in 0..count {
            let source = {
                let state = self
                    .runtime
                    .inventory
                    .try_lock()
                    .map_err(|_| "Source inventory is busy")?;
                let registry = &state
                    .as_ref()
                    .ok_or("Source inventory unavailable")?
                    .checkpoints;
                if registry.ambiguous || registry.epoch != epoch {
                    return Err("Checkpoint registrations changed; sources retained".into());
                }
                registry.entries[at]
                    .sources
                    .iter()
                    .find(|source| source.store().same_owner(store))
                    .cloned()
            };
            let Some(source) = source else { continue };
            self.check_owned_checkpoint_source(&source, &|| self.check())?;
            needed.iter_mut().for_each(|(_, covered)| *covered = false);
            source
                .visit_blocks(
                    &mut || {
                        self.check()
                            .map_err(|_| crate::large_document::Error::Changed)
                    },
                    |reference| {
                        if let Ok(at) =
                            needed.binary_search_by_key(&reference.fingerprint(), |(key, _)| *key)
                        {
                            needed[at].1 = true;
                        }
                        Ok(())
                    },
                )
                .map_err(|e| e.to_string())?;
            let unchanged = {
                let state = self
                    .runtime
                    .inventory
                    .try_lock()
                    .map_err(|_| "Source inventory is busy")?;
                let registry = &state
                    .as_ref()
                    .ok_or("Source inventory unavailable")?
                    .checkpoints;
                !registry.ambiguous && registry.epoch == epoch
            };
            if !unchanged {
                return Err("Checkpoint registrations changed; sources retained".into());
            }
            if needed.iter().all(|(_, covered)| *covered) {
                return Ok(Some(source));
            }
        }
        Ok(None)
    }
    /// Re-read the exact registered marker before off-lock recovery adoption.
    /// The in-memory registry supplies the digest; a caller cannot supply one.
    pub(crate) fn recheck_checkpoint(
        &self,
        marker: &mut NativeMarker<'_>,
        check: &dyn Fn() -> Result<(), String>,
    ) -> Result<(), String> {
        check()?;
        marker.check_session(self)?;
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
                return Err("Checkpoint registration is ambiguous; sources retained".into());
            }
            let at = registry
                .entries
                .binary_search_by_key(&marker.snapshot(), |e| e.marker.snapshot)
                .map_err(|_| "Recovery marker has no validated checkpoint registration")?;
            let entry = &registry.entries[at];
            Observation {
                marker: entry.marker,
                sha256: entry.sha256,
            }
        };
        if marker.verify_digest(expected.sha256, check)? != expected.marker {
            return Err("Recovery checkpoint changed before registration; sources retained".into());
        }
        check()?;
        self.check()?;
        let state = self
            .runtime
            .inventory
            .try_lock()
            .map_err(|_| "Source inventory is busy")?;
        let registry = &state
            .as_ref()
            .ok_or("Source inventory unavailable")?
            .checkpoints;
        if registry.ambiguous
            || !registry
                .entries
                .iter()
                .any(|e| e.marker == expected.marker && e.sha256 == expected.sha256)
        {
            return Err("Recovery checkpoint registration changed; sources retained".into());
        }
        Ok(())
    }
    pub(crate) fn check_owned_checkpoint_source(
        &self,
        source: &Snapshot,
        check: &dyn Fn() -> Result<(), String>,
    ) -> Result<(), String> {
        check()?;
        self.check()?;
        source
            .check_owner(&mut || {
                self.check()
                    .map_err(|_| crate::large_document::Error::Changed)
            })
            .map_err(|e| e.to_string())?;
        let owner = source.store();
        let state = self
            .runtime
            .inventory
            .try_lock()
            .map_err(|_| "Source inventory is busy")?;
        let location = state
            .as_ref()
            .and_then(|inventory| inventory.locations.get(&owner.identity()))
            .ok_or("Checkpoint source is outside the authenticated inventory")?;
        let known = location.weak.as_ref().and_then(super::WeakStore::upgrade);
        if !known.is_some_and(|known| known.same_owner(&owner))
            || owner.filesystem_identity() != (location.directory_identity, location.file_identity)
        {
            return Err("Checkpoint source differs from its registered physical owner".into());
        }
        Ok(())
    }
    pub(crate) fn register_checkpoint(
        &self,
        proof: CheckpointProof,
        marker: &mut NativeMarker<'_>,
        check: &dyn Fn() -> Result<(), String>,
    ) -> Result<(), String> {
        check()?;
        self.check()?;
        marker.check_session(self)?;
        let parts = proof.into_parts();
        if parts.snapshot != marker.snapshot() || parts.bytes != marker.len() {
            return Err("Checkpoint marker differs from complete proof".into());
        }
        for source in &parts.sources {
            check()?;
            if !source.store().publication_marked()
                || !source.store().resident_budget().same_pool(&self.resident())
                || !source.store().uses_disk_budget(&self.disk())
            {
                return Err("Checkpoint source is unpublished or outside admission".into());
            }
            self.check_owned_checkpoint_source(source, check)?;
        }
        let observed = marker.verify_digest(parts.sha256, check)?;
        for source in &parts.sources {
            self.check_owned_checkpoint_source(source, check)?;
        }
        check()?;
        self.check()?;
        let mut state = self
            .runtime
            .inventory
            .try_lock()
            .map_err(|_| "Source inventory is busy")?;
        let inventory = state.as_mut().ok_or("Source inventory unavailable")?;
        let path = self.directory.join(format!(
            "recovery-v3-{}.pbr",
            uuid::Uuid::from_bytes(parts.snapshot)
        ));
        if !inventory.files.iter().any(|held| {
            held.path == path
                && held.identity == observed.identity
                && held.bytes == observed.bytes
                && held.modified == observed.modified
        }) {
            return Err("Checkpoint marker is outside the exact held inventory".into());
        }
        for source in &parts.sources {
            let known = inventory
                .locations
                .get(&source.store().identity())
                .and_then(|location| location.weak.as_ref())
                .and_then(super::WeakStore::upgrade);
            if !known.is_some_and(|known| known.same_owner(&source.store())) {
                return Err("Checkpoint source differs from its registered owner".into());
            }
        }
        if let Some((at, epoch)) = inventory.checkpoints.prepare(observed, &parts)? {
            inventory
                .checkpoints
                .insert_prepared(at, epoch, observed, parts);
        }
        drop(state);
        self.check()?;
        check()
    }
    /// Full marker census only. No caller may turn this count into a deletion
    /// permit: live root integration and a source-specific retirement gate remain closed.
    pub(crate) fn checkpoint_census(
        &self,
        check: &dyn Fn() -> Result<(), String>,
    ) -> Result<CheckpointCensus, String> {
        check()?;
        self.check()?;
        let maximum = self.runtime.limits.max_files;
        let charge = self
            .resident()
            .reserve_bytes(
                maximum
                    .checked_mul(2 * std::mem::size_of::<Observation>())
                    .and_then(|n| n.checked_add(128))
                    .ok_or("Checkpoint observation footprint overflow")?,
                64 * 1024 * 1024,
            )
            .map_err(|e| e.to_string())?;
        let mut entries = Vec::new();
        entries
            .try_reserve_exact(maximum)
            .map_err(|_| "Checkpoint observation allocation refused")?;
        if entries.capacity() > maximum.saturating_mul(2) {
            return Err("Checkpoint observation capacity differs".into());
        }
        let epoch = {
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
            for entry in &registry.entries {
                entries.push(Observation {
                    marker: entry.marker,
                    sha256: entry.sha256,
                });
            }
            registry.epoch
        };
        let observations = Observations {
            entries,
            _charge: charge,
            epoch,
        };
        let first = self.checkpoint_members(check)?;
        let journals = first
            .entries
            .iter()
            .filter(|member| member.kind == 3)
            .count();
        if self.publication_member_count()? != journals {
            return Err("Publication registration differs from complete namespace census".into());
        }
        let mut count = 0;
        for member in &first.entries {
            if matches!(member.kind, 2 | 3) {
                continue;
            }
            let end = member
                .name
                .iter()
                .position(|byte| *byte == 0)
                .unwrap_or(member.name.len());
            let name = std::str::from_utf8(&member.name[..end])
                .map_err(|_| "Unknown checkpoint name retained")?;
            if name == "owner.pbr" {
                continue;
            }
            let id = name
                .strip_prefix("recovery-v3-")
                .and_then(|n| n.strip_suffix(".pbr"))
                .and_then(|n| uuid::Uuid::parse_str(n).ok())
                .map(|id| *id.as_bytes())
                .ok_or("Unknown, incomplete or unsupported recovery marker retained")?;
            let at = observations
                .entries
                .binary_search_by_key(&id, |entry| entry.marker.snapshot)
                .map_err(|_| "Unregistered recovery marker retained")?;
            let expected = observations.entries[at];
            if member.identity != expected.marker.identity {
                return Err("Checkpoint marker owner changed; roots retained".into());
            }
            let mut marker = self.open_recovery_marker(id)?;
            if marker.verify_digest(expected.sha256, check)? != expected.marker {
                return Err("Checkpoint observation changed; roots retained".into());
            }
            count += 1;
        }
        if count != observations.entries.len() {
            return Err("Registered checkpoint is missing; roots retained".into());
        }
        let second = self.checkpoint_members(check)?;
        if first.entries != second.entries {
            return Err("Checkpoint namespace membership changed; roots retained".into());
        }
        if self.publication_member_count()? != journals {
            return Err("Publication registrations changed during census".into());
        }
        check()?;
        self.check()?;
        let state = self
            .runtime
            .inventory
            .try_lock()
            .map_err(|_| "Source inventory is busy")?;
        let registry = &state
            .as_ref()
            .ok_or("Source inventory unavailable")?
            .checkpoints;
        if registry.ambiguous || registry.epoch != observations.epoch {
            return Err("Checkpoint registrations changed during census".into());
        }
        Ok(CheckpointCensus { registered: count })
    }
    fn checkpoint_members(
        &self,
        check: &dyn Fn() -> Result<(), String>,
    ) -> Result<Members, String> {
        let maximum = self
            .runtime
            .limits
            .max_files
            .checked_mul(2)
            .and_then(|n| n.checked_add(1))
            .ok_or("Checkpoint member bound overflow")?;
        let charge = self
            .resident()
            .reserve_bytes(
                maximum
                    .checked_mul(2 * std::mem::size_of::<Member>())
                    .and_then(|n| n.checked_add(128))
                    .ok_or("Checkpoint member footprint overflow")?,
                64 * 1024 * 1024,
            )
            .map_err(|e| e.to_string())?;
        let mut entries = Vec::new();
        entries
            .try_reserve_exact(maximum)
            .map_err(|_| "Checkpoint member allocation refused")?;
        if entries.capacity() > maximum.saturating_mul(2) {
            return Err("Checkpoint member capacity differs".into());
        }
        for entry in
            fs::read_dir(&self.directory).map_err(|_| "Cannot inspect checkpoint namespace")?
        {
            check()?;
            self.check()?;
            if entries.len() == maximum {
                return Err("Checkpoint namespace member limit exceeded".into());
            }
            let entry = entry.map_err(|_| "Cannot inspect checkpoint member")?;
            let path = entry.path();
            let name = entry
                .file_name()
                .into_string()
                .map_err(|_| "Unknown checkpoint namespace name retained")?;
            if name.is_empty() || name.len() >= 80 || !name.is_ascii() {
                return Err("Unknown checkpoint namespace name retained".into());
            }
            let metadata =
                fs::symlink_metadata(&path).map_err(|_| "Checkpoint member unavailable")?;
            let mut journal_sha256 = [0; 32];
            let (kind, identity, child) = if metadata.is_dir() && !metadata.file_type().is_symlink()
            {
                let directory = Directory::open(&path)?;
                let expected = {
                    let state = self
                        .runtime
                        .inventory
                        .try_lock()
                        .map_err(|_| "Source inventory is busy")?;
                    let inventory = state.as_ref().ok_or("Source inventory unavailable")?;
                    inventory
                        .locations
                        .values()
                        .find(|location| location.path == path)
                        .map(|location| (location.directory_identity, location.file_identity))
                        .or_else(|| {
                            inventory
                                .staging
                                .values()
                                .find(|location| location.path == path)
                                .map(|location| location.filesystem_identity())
                        })
                        .ok_or("Unregistered source namespace retained")?
                };
                if directory.identity() != expected.0 {
                    return Err("Source namespace owner changed; roots retained".into());
                }
                let filename = match name.split_once('-').map(|pair| pair.0) {
                    Some("source") => "blocks.pbt",
                    Some("bytes") => "bytes.pbb",
                    Some("planar") => "planes.pbp",
                    Some("scalar") => "field.pbf",
                    Some("segments") => "segments.pbj",
                    _ => return Err("Unknown source namespace retained".into()),
                };
                let mut files = fs::read_dir(directory.path())
                    .map_err(|_| "Cannot inspect owned source namespace")?;
                let file = files
                    .next()
                    .ok_or("Incomplete source namespace retained")?
                    .map_err(|_| "Source member unavailable")?;
                if file.file_name() != filename || files.next().is_some() {
                    return Err("Unknown source namespace contents retained".into());
                }
                let file = crate::file_safety::open_native_identity(&file.path())
                    .map_err(|_| "Source namespace member unavailable")?;
                if crate::file_safety::identity(&file)? != expected.1
                    || crate::file_safety::link_count(&file)? != 1
                {
                    return Err("Source namespace member owner changed".into());
                }
                directory.check()?;
                (2, expected.0, expected.1)
            } else if metadata.is_file() && !metadata.file_type().is_symlink() {
                let file = crate::file_safety::open_native_identity(&path)
                    .map_err(|_| "Checkpoint file unavailable")?;
                if crate::file_safety::link_count(&file)? != 1 {
                    return Err("Checkpoint alias retained".into());
                }
                (1, crate::file_safety::identity(&file)?, (0, 0))
            } else if super::publication::recognized_name(&name) {
                let file = crate::file_safety::open_native_identity(&path)
                    .map_err(|_| "Publication member unavailable")?;
                let identity = crate::file_safety::identity(&file)?;
                journal_sha256 = self.check_publication_member(&name, identity, check)?;
                (3, identity, (0, 0))
            } else {
                return Err("Unknown checkpoint member type retained".into());
            };
            let mut key = [0; 80];
            key[..name.len()].copy_from_slice(name.as_bytes());
            entries.push(Member {
                name: key,
                kind,
                identity,
                child,
                journal_sha256,
            });
        }
        entries.sort_unstable();
        check()?;
        self.check()?;
        Ok(Members {
            entries,
            _charge: charge,
        })
    }
}
