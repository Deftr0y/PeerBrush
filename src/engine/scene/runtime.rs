//! Bind derived scene workers to the actual persistent, instance-admitted service.
use super::{StageRuntime, StageSession};
use crate::{
    large_document::{BlockRef, DiskBudget, ResidentBudget, Snapshot, Store},
    workspace::source_runtime::{SourceRuntime, SourceSession},
};
use std::sync::Arc;

impl StageRuntime for Arc<SourceRuntime> {
    type Session = SourceSession;
    fn resident(&self) -> ResidentBudget {
        SourceRuntime::resident(self)
    }
    fn disk(&self) -> DiskBudget {
        SourceRuntime::disk(self)
    }
    fn begin(&self) -> Result<SourceSession, String> {
        SourceRuntime::begin(self)
    }
}
impl StageSession for SourceSession {
    fn check(&self) -> Result<(), String> {
        SourceSession::check(self)
    }
    fn store_for_lineage(&self, lineage: [u8; 16]) -> Result<Store, String> {
        SourceSession::store_for_lineage(self, lineage)
    }
    fn seal_store(
        &self,
        store: &Store,
        blocks: &[BlockRef],
        metadata: &[u8],
    ) -> Result<Snapshot, String> {
        SourceSession::seal_store(self, store, blocks, metadata)
    }
}
