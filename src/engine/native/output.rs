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
pub(super) struct Retirement {
    fragments: Vec<FreshFragment>,
    _charge: Option<Reservation>,
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
            refs.sort_unstable_by_key(|r| r.fingerprint());
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
        self.into_retirement().abort(session, cleanup)
    }
    fn into_retirement(self) -> Retirement {
        let Self {
            surface,
            seals,
            fragments,
            charge,
        } = self;
        drop(surface);
        drop(seals);
        Retirement {
            fragments,
            _charge: charge,
        }
    }
}
impl Retirement {
    pub(super) fn retire_selected(
        self,
        flags: &mut std::slice::Iter<'_, bool>,
        session: &impl StageSession,
        cleanup: &dyn Fn() -> Result<()>,
    ) -> Result<()> {
        let mut error = None;
        for fragment in self.fragments {
            match flags.next() {
                Some(true) => drop(fragment),
                Some(false) => {
                    if let Err(reason) = session
                        .cleanup_check()
                        .and_then(|_| cleanup())
                        .and_then(|_| session.abort_fragment(fragment, cleanup))
                    {
                        error.get_or_insert(reason);
                    }
                }
                None => {
                    drop(fragment);
                    error.get_or_insert(
                        "Native retirement flags are incomplete; bytes remain charged".into(),
                    );
                }
            }
        }
        error.map_or(Ok(()), Err)
    }
    fn abort(self, session: &impl StageSession, cleanup: &dyn Fn() -> Result<()>) -> Result<()> {
        let mut error = None;
        for fragment in self.fragments {
            let result = session
                .cleanup_check()
                .and_then(|_| cleanup())
                .and_then(|_| session.abort_fragment(fragment, cleanup));
            if let Err(reason) = result {
                error.get_or_insert(reason);
            }
        }
        error.map_or(Ok(()), Err)
    }
}
/// Release every output view/checkpoint before the first unique-owner retire.
pub(super) fn abort_outputs(
    outputs: Vec<StagedOutput>,
    session: &impl StageSession,
    cleanup: &dyn Fn() -> Result<()>,
) -> Result<()> {
    let mut retired = Vec::new();
    // Every sealed output retains its 4KiB metadata envelope through this array.
    retired
        .try_reserve_exact(outputs.len())
        .map_err(|_| "Native retirement allocation refused; provisional bytes remain charged")?;
    for output in outputs {
        retired.push(output.draft.into_retirement());
    }
    let mut error = None;
    for retirement in retired {
        if let Err(reason) = retirement.abort(session, cleanup) {
            error.get_or_insert(reason);
        }
    }
    error.map_or(Ok(()), Err)
}
impl StagedOutput {
    pub(super) fn fragments(&self) -> impl ExactSizeIterator<Item = &FreshFragment> {
        self.draft.fragments.iter()
    }
    pub(super) fn into_retirement(self) -> Retirement {
        self.draft.into_retirement()
    }
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
        draft_with(base, &mut |_| {})
    }
    fn draft_with(
        base: &NativeDocument,
        edit: &mut dyn FnMut(&mut [u8]),
    ) -> (OutputDraft, std::path::PathBuf) {
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
        edit(&mut bytes);
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
    #[test]
    fn all_shared_store_output_views_drop_before_unique_fragment_retirement() {
        let pool = ResidentBudget::new(8 * 1024 * 1024).unwrap();
        let base = super::super::tests::document(&pool);
        let (first, path) = draft(&base);
        let second = OutputDraft::new(first.surface.clone(), vec![]);
        let first = first
            .seal(&base, &Session, &|| Ok(()))
            .unwrap_or_else(|e| panic!("{}", e.reason()));
        let second = second
            .seal(&base, &Session, &|| Ok(()))
            .unwrap_or_else(|e| panic!("{}", e.reason()));
        let baseline = pool.used();
        assert!(base.disk_budget().used().unwrap() > 0);
        abort_outputs(vec![first, second], &Session, &|| Ok(())).unwrap();
        assert_eq!(base.disk_budget().used().unwrap(), 0);
        assert!(pool.used() < baseline);
        std::fs::remove_dir(path).unwrap();
    }
    #[test]
    fn retained_noop_and_unshifted_expansion_keep_original_across_distinct_owned_stores() {
        use crate::engine::scene::{PendingScene, Retained36};
        let pool = ResidentBudget::new(16 * 1024 * 1024).unwrap();
        let blank = super::super::tests::document(&pool);
        let blank = NativeDocument {
            core: blank
                .core
                .map_unique(|mut core, _| {
                    core.disk = DiskBudget::new(8 * 1024 * 1024, 64).unwrap();
                    Ok(core)
                })
                .unwrap(),
        };
        let diagnostic_disk = blank.disk_budget().clone();
        // 1..4 initial source; 10..19 equal/expand/shrink; 20..24 hidden padding;
        // 30 final resident release. ok=0 is the expected refused branch.
        macro_rules! diagnostic {
            ($step:expr, $width:expr, $ok:expr) => {
                eprintln!(
                    "PB_RETAINED_DIAG step={} width={} resident={} disk={} ok={}",
                    $step,
                    $width,
                    pool.used(),
                    diagnostic_disk.used().unwrap_or(u64::MAX),
                    $ok
                );
            };
        }
        diagnostic!(1, 0, 1);
        let (first, path) = draft(&blank);
        diagnostic!(2, 0, 1);
        let first = first
            .seal(&blank, &Session, &|| Ok(()))
            .unwrap_or_else(|e| panic!("{}", e.reason()));
        diagnostic!(3, 0, 1);
        let stored = first.surface().clone();
        first.mark_published();
        let metadata_owner = MetadataCharge::reserve(&pool, 64 * 1024, &|| Ok(())).unwrap();
        let mut metadata = blank.core.metadata.clone();
        metadata.layers[0].pixels.retained = Some(Box::new(Retained36 {
            pixels: RasterRef36 {
                id: 2,
                width: 8,
                height: 8,
                depth: 16,
                retained: None,
            },
            matrix: crate::retained::IDENTITY,
            projective: None,
            warps: vec![],
        }));
        let pending = PendingScene::new(
            Admitted::from_parts(Arc::new(metadata), metadata_owner),
            RasterRoster::new(vec![stored, Surface::zero(8, 8, 16).unwrap()]).unwrap(),
            pool.clone(),
            blank.disk_budget().clone(),
        )
        .unwrap();
        let base = NativeDocument::from_pending(pending).unwrap();
        diagnostic!(4, 0, 1);
        let target = base.metadata().layer(0).unwrap().pixels().slot();
        let original = base
            .metadata()
            .layer(0)
            .unwrap()
            .pixels()
            .retained()
            .unwrap()
            .pixels()
            .slot();
        let original_key = base.source(original).unwrap().key();
        // Published files remain in the non-refunding ledger. Keep their names
        // alive until all source creation finishes so Unix inode reuse cannot
        // make a later fixture alias a deliberately retained identity.
        let mut published_paths = Vec::with_capacity(2);
        for width in [8, 16, 4] {
            diagnostic!(10, width, 1);
            let (mut next, next_path) = draft(&base);
            diagnostic!(11, width, 1);
            let tile = next.surface.physical_tiles().next().unwrap();
            let owner = tile.owner().clone();
            let reference = tile.reference();
            drop(tile);
            next.surface = Surface::from_plane(
                Plane::from_blocks(
                    width,
                    width,
                    16,
                    owner,
                    BTreeMap::from([((0, 0), reference)]),
                )
                .unwrap(),
            )
            .unwrap();
            diagnostic!(12, width, 1);
            let output = next
                .seal(&base, &Session, &|| Ok(()))
                .unwrap_or_else(|e| panic!("{}", e.reason()));
            diagnostic!(13, width, 1);
            assert_ne!(base.source(target).unwrap().key(), output.surface().key());
            let patch = RasterPatch {
                target,
                output,
                retained: RetainedUpdate::Keep,
                layer_origin: None,
            };
            let prepared = base.capture_edit().unwrap().raster_patch(patch, &|| Ok(()));
            diagnostic!(14, width, u8::from(prepared.is_ok()));
            if width >= 8 {
                let prepared = prepared.unwrap_or_else(|e| panic!("{}", e.reason()));
                let current = base.publish("human", &[], 1, false, prepared).unwrap();
                diagnostic!(15, width, 1);
                let raster = current.metadata().layer(0).unwrap().pixels();
                assert_eq!(raster.width(), width);
                assert_eq!(
                    current
                        .source(raster.retained().unwrap().pixels().slot())
                        .unwrap()
                        .key(),
                    original_key
                );
                drop(current);
                diagnostic!(16, width, 1);
                published_paths.push(next_path);
                diagnostic!(17, width, 1);
            } else {
                let refused = prepared.err().unwrap();
                assert!(refused.reason().contains("source reconciliation"));
                refused.abort(&Session, &|| Ok(())).unwrap();
                diagnostic!(18, width, 0);
                std::fs::remove_dir(next_path).unwrap();
                diagnostic!(19, width, 0);
            }
        }
        // Hidden native RGB in physical padding is part of exact equivalence.
        diagnostic!(20, 0, 1);
        let (changed, changed_path) =
            draft_with(&base, &mut |bytes| bytes[(200 * 256 + 200) * 8] = 1);
        let output = changed
            .seal(&base, &Session, &|| Ok(()))
            .unwrap_or_else(|e| panic!("{}", e.reason()));
        diagnostic!(21, 0, 1);
        let refused = base
            .capture_edit()
            .unwrap()
            .raster_patch(
                RasterPatch {
                    target,
                    output,
                    retained: RetainedUpdate::Keep,
                    layer_origin: None,
                },
                &|| Ok(()),
            )
            .err()
            .unwrap();
        assert!(refused.reason().contains("source reconciliation"));
        diagnostic!(22, 0, 0);
        refused.abort(&Session, &|| Ok(())).unwrap();
        diagnostic!(23, 0, 0);
        std::fs::remove_dir(changed_path).unwrap();
        diagnostic!(24, 0, 0);
        drop(base);
        drop(blank);
        for published in published_paths {
            std::fs::remove_dir_all(published).unwrap();
        }
        std::fs::remove_dir_all(path).unwrap();
        diagnostic!(30, 0, 1);
        assert_eq!(pool.used(), 0);
    }
}
