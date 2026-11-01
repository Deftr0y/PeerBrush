//! Provisional generated pixels. File staging never grants document publication.
use super::*;
use crate::{
    engine::scene::StageSession,
    large_document::{BlockRef, FreshFragment, Reservation, Snapshot, Store},
};
use std::collections::BTreeMap;

pub struct OutputDraft {
    surface: Surface,
    seals: Vec<Snapshot>,
    fragments: Vec<FreshFragment>,
    charge: Option<Reservation>,
}
pub struct StagedOutput {
    draft: OutputDraft,
}
#[must_use]
pub struct RefusedOutput {
    reason: String,
    draft: OutputDraft,
}
impl RefusedOutput {
    pub fn reason(&self) -> &str {
        &self.reason
    }
    pub fn into_draft(self) -> OutputDraft {
        self.draft
    }
}
impl OutputDraft {
    pub fn new(surface: Surface, fragments: Vec<FreshFragment>) -> Self {
        Self {
            surface,
            seals: vec![],
            fragments,
            charge: None,
        }
    }
    /// A failed seal returns every owned source token for cleanup outside locks.
    pub(crate) fn seal(
        mut self,
        base: &NativeDocument,
        session: &impl StageSession,
        check: &dyn Fn() -> Result<()>,
    ) -> std::result::Result<StagedOutput, RefusedOutput> {
        match self.seal_inner(base, session, check) {
            Ok(()) => Ok(StagedOutput { draft: self }),
            Err(reason) => Err(RefusedOutput {
                reason,
                draft: self,
            }),
        }
    }
    fn seal_inner(
        &mut self,
        base: &NativeDocument,
        session: &impl StageSession,
        check: &dyn Fn() -> Result<()>,
    ) -> Result<()> {
        session.check()?;
        check()?;
        if !self.surface.uses_budget(base.budget())
            || !self.surface.uses_disk_budget(base.disk_budget())
        {
            return Err("Generated pixels use another source admission".into());
        }
        let count = self.surface.physical_tiles().count();
        if count == 0 && (!self.surface.is_known_zero() || !self.fragments.is_empty()) {
            return Err(
                "Empty output must be explicit zero after unused fragments are retired".into(),
            );
        }
        let bytes = count
            .checked_mul(std::mem::size_of::<BlockRef>() + 256)
            .and_then(|n| {
                self.fragments
                    .capacity()
                    .checked_mul(std::mem::size_of::<FreshFragment>())
                    .and_then(|f| n.checked_add(f))
            })
            .and_then(|n| n.checked_add(4096))
            .ok_or("Generated source index footprint overflow")?;
        self.charge = Some(
            base.budget()
                .reserve_bytes(bytes, bytes)
                .map_err(|e| e.to_string())?,
        );
        let mut stores: BTreeMap<[u8; 16], (Store, Vec<BlockRef>)> = BTreeMap::new();
        for tile in self.surface.physical_tiles() {
            session.check()?;
            check()?;
            let entry = stores
                .entry(tile.owner().identity())
                .or_insert_with(|| (tile.owner().clone(), vec![]));
            entry
                .1
                .try_reserve_exact(1)
                .map_err(|_| "Generated source seal allocation refused")?;
            entry.1.push(tile.reference());
        }
        if self
            .fragments
            .iter()
            .any(|fragment| !stores.contains_key(&fragment.store().identity()))
        {
            return Err("Unused generated fragment must be retired before sealing".into());
        }
        self.seals.clear();
        self.seals
            .try_reserve_exact(stores.len())
            .map_err(|_| "Generated checkpoint allocation refused")?;
        for (_, (store, mut refs)) in stores {
            refs.sort_unstable_by_key(|r| r.digest());
            refs.dedup();
            self.seals.push(session.seal_store(
                &store,
                &refs,
                b"PeerBrush provisional native output v1",
            )?);
        }
        session.check()?;
        check()?;
        Ok(())
    }
    /// First release all candidate/preview clones. Refused or late ownership
    /// remains charged; no foreign replacement can be retired.
    pub(crate) fn abort(
        self,
        session: &impl StageSession,
        cleanup: &dyn Fn() -> Result<()>,
    ) -> Result<()> {
        let Self {
            surface,
            seals,
            fragments,
            charge,
        } = self;
        drop(surface);
        drop(seals);
        let mut error = None;
        for fragment in fragments {
            let result = session
                .cleanup_check()
                .and_then(|_| cleanup())
                .and_then(|_| session.abort_fragment(fragment, cleanup));
            if let Err(reason) = result {
                error.get_or_insert(reason);
            }
        }
        drop(charge);
        error.map_or(Ok(()), Err)
    }
}
impl StagedOutput {
    pub fn surface(&self) -> &Surface {
        &self.draft.surface
    }
    /// Producer supersession/cancellation retires only owned provisional files.
    pub(crate) fn abort(
        self,
        session: &impl StageSession,
        cleanup: &dyn Fn() -> Result<()>,
    ) -> Result<()> {
        self.draft.abort(session, cleanup)
    }
    /// Final Engine path only, after guards. No callbacks or file operations.
    pub(super) fn mark_published(self) {
        for fragment in self.draft.fragments {
            let _ = fragment.publish();
        }
    }
    pub(super) fn into_draft(self) -> OutputDraft {
        self.draft
    }
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;
    use crate::{
        large_document::{Limits, TileSpec},
        raster::backed::Plane,
    };
    pub(crate) struct Session;
    impl StageSession for Session {
        fn check(&self) -> Result<()> {
            Ok(())
        }
        fn store_for_lineage(&self, _: [u8; 16]) -> Result<Store> {
            Err("No fixture source creation".into())
        }
        fn seal_store(
            &self,
            store: &Store,
            refs: &[BlockRef],
            metadata: &[u8],
        ) -> Result<Snapshot> {
            let parent = store.checkpoint_reference().map_err(|e| e.to_string())?;
            store
                .commit(
                    parent.map_or(store.identity(), |p| p.source()),
                    parent.map_or(1, |p| p.revision() + 1),
                    parent,
                    metadata,
                    refs,
                    &mut || Ok(()),
                )
                .map_err(|e| e.to_string())
        }
        fn abort_fragment(
            &self,
            fragment: FreshFragment,
            cleanup: &dyn Fn() -> Result<()>,
        ) -> Result<()> {
            fragment
                .abort(&mut || cleanup().map_err(|_| crate::large_document::Error::Cancelled))
                .map_err(|e| e.to_string())
        }
    }
    pub(crate) fn draft(base: &NativeDocument) -> (OutputDraft, std::path::PathBuf) {
        let path = std::fs::canonicalize(std::env::temp_dir())
            .unwrap()
            .join(format!("peerbrush-output-draft-{}", crate::engine::id()));
        let fragment = FreshFragment::create(
            &path,
            base.budget().clone(),
            base.disk_budget().clone(),
            Limits {
                disk_bytes: 1024 * 1024,
                max_blocks: 16,
                max_resident_blocks: 8,
                manifest_bytes: 64 * 1024,
            },
        )
        .unwrap();
        let spec = TileSpec::new(256, 256, 16).unwrap();
        let mut bytes = ScratchBytes::new(base.budget(), spec.bytes(), spec.bytes()).unwrap();
        for (pair, word) in bytes[..8]
            .chunks_exact_mut(2)
            .zip([12345u16, 23456, 34567, 65535])
        {
            pair.copy_from_slice(&word.to_be_bytes());
        }
        let reference = fragment.store().put(spec, &bytes, &mut || Ok(())).unwrap();
        drop(bytes);
        let surface = Surface::from_plane(
            Plane::from_blocks(
                8,
                8,
                16,
                fragment.store().clone(),
                BTreeMap::from([((0, 0), reference)]),
            )
            .unwrap(),
        )
        .unwrap();
        (OutputDraft::new(surface, vec![fragment]), path)
    }
    #[test]
    fn seal_refusal_returns_all_tokens_and_cleanup_releases_owned_bytes() {
        let pool = ResidentBudget::new(8 * 1024 * 1024).unwrap();
        let base = super::super::tests::document(&pool);
        let (draft, path) = draft(&base);
        let baseline = pool.used();
        let refused = draft
            .seal(&base, &Session, &|| Err("cancelled".into()))
            .err()
            .unwrap();
        assert_eq!(refused.reason(), "cancelled");
        assert_eq!(pool.used(), baseline);
        assert!(base.disk_budget().used().unwrap() > 0);
        refused.into_draft().abort(&Session, &|| Ok(())).unwrap();
        assert_eq!(base.disk_budget().used().unwrap(), 0);
        assert!(pool.used() < baseline);
        std::fs::remove_dir(&path).unwrap();
    }
    #[test]
    fn completed_output_seals_and_can_abort_after_releasing_its_surface_and_checkpoint() {
        let pool = ResidentBudget::new(8 * 1024 * 1024).unwrap();
        let base = super::super::tests::document(&pool);
        let (draft, path) = draft(&base);
        let staged = draft
            .seal(&base, &Session, &|| Ok(()))
            .unwrap_or_else(|failure| panic!("{}", failure.reason()));
        assert_eq!(staged.draft.seals.len(), 1);
        staged.into_draft().abort(&Session, &|| Ok(())).unwrap();
        assert_eq!(base.disk_budget().used().unwrap(), 0);
        std::fs::remove_dir(path).unwrap();
    }
}
