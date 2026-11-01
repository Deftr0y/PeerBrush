//! Guarded Source36 save result. Fields are private; generic callbacks, checksums
//! or individual chunk completions cannot manufacture publication authority.
use super::{bulk36::Digest, standard36::ComparedStandard};
use crate::{
    engine::scene::SceneSnapshot,
    large_document::{ByteRole, CompletedBytes, Error, ScratchBytes},
};
use ring::digest::{Context, SHA256};
use std::io::Read;

/// Only the full scene save driver may construct this after reopened complete
/// source/native stream validation and exact standard metadata/sample fidelity.
/// Retaining the snapshot keeps its metadata, Surface owners and leases alive.
pub(crate) struct CompletedSave36<'a> {
    scene: &'a SceneSnapshot,
    candidate: CompletedBytes,
    staging: Vec<CompletedBytes>,
    bytes: u64,
    sha256: Digest,
    provenance: Digest,
    core_sha256: Digest,
    _standard_fidelity: ComparedStandard,
}
impl CompletedSave36<'_> {
    pub(crate) fn scene(&self) -> &SceneSnapshot {
        self.scene
    }
    pub(crate) fn candidate(&self) -> &CompletedBytes {
        &self.candidate
    }
    pub(crate) fn bytes(&self) -> u64 {
        self.bytes
    }
    pub(crate) fn sha256(&self) -> Digest {
        self.sha256
    }
    pub(crate) fn provenance(&self) -> Digest {
        self.provenance
    }
    pub(crate) fn core_sha256(&self) -> Digest {
        self.core_sha256
    }
    /// Reobserve the entire owned candidate immediately before destination copy.
    /// Source file identity/lease and engine revision are the caller's guard;
    /// each supplied error is preserved through the owned I/O callback.
    pub(crate) fn recheck(&self, check: &dyn Fn() -> Result<(), String>) -> Result<(), String> {
        check()?;
        if super::scene_save36::provenance(self.scene) != self.provenance
            || self.candidate.role() != ByteRole::PhotoshopContainer
            || self.candidate.binding() != self.core_sha256
            || !self
                .candidate
                .uses_budgets(self.scene.budget(), self.scene.disk_budget())
            || self.candidate.payload_length() != self.bytes
            || self.candidate.payload_sha256() != self.sha256
        {
            return Err(
                "Source36 save candidate differs from its captured source/owned completion".into(),
            );
        }
        let mut transfer =
            ScratchBytes::new(self.scene.budget(), 65536, 65536).map_err(|e| e.to_string())?;
        let mut guarded = || check().map_err(|e| Error::Io(std::io::Error::other(e)));
        let mut input = self
            .candidate
            .cursor(0, self.bytes, &mut guarded)
            .map_err(|e| e.to_string())?;
        let mut left = self.bytes;
        let mut hash = Context::new(&SHA256);
        while left != 0 {
            check()?;
            let n = left.min(65536) as usize;
            input
                .read_exact(&mut transfer[..n])
                .map_err(|e| e.to_string())?;
            hash.update(&transfer[..n]);
            left -= n as u64;
        }
        if input.read(&mut [0; 1]).map_err(|e| e.to_string())? != 0
            || hash.finish().as_ref() != &self.sha256[..]
        {
            return Err("Source36 save candidate bytes changed after qualification".into());
        }
        check()
    }
    /// Call only after destination publication/abort no longer needs staging.
    /// Verified retirement never removes a foreign replacement. Failed remains
    /// charged in the persistent operation inventory for admitted later cleanup.
    pub(crate) fn retire(self, check: &dyn Fn() -> Result<(), String>) -> Result<(), String> {
        let mut first = None;
        for file in self
            .staging
            .into_iter()
            .chain(std::iter::once(self.candidate))
        {
            let mut guarded = || check().map_err(|e| Error::Io(std::io::Error::other(e)));
            if let Err(error) = file.retire(&mut guarded) {
                if first.is_none() {
                    first = Some(error.to_string());
                }
            }
        }
        first.map_or(Ok(()), Err)
    }
}
