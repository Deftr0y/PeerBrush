//! Side-qualified source resolution and owned provisional task pixels.
use super::{metadata_inverse::*, *};
use crate::{
    engine::scene::StageSession,
    large_document::{FreshFragment, TileSpec},
    raster::surface::TilePatch,
};

pub(super) struct Sources<'a, S: StageSession> {
    before: &'a NativeDocument,
    after: &'a NativeDocument,
    current: &'a NativeDocument,
    session: &'a S,
    check: &'a dyn Fn() -> Result<()>,
    prepared: Vec<Surface>,
    imported: Vec<Surface>,
    outputs: Vec<StagedOutput>,
    pending: Option<FreshFragment>,
    refused: Option<OutputDraft>,
    _charge: Reservation,
}
impl<'a, S: StageSession> Sources<'a, S> {
    pub(super) fn new(
        before: &'a NativeDocument,
        after: &'a NativeDocument,
        current: &'a NativeDocument,
        session: &'a S,
        check: &'a dyn Fn() -> Result<()>,
    ) -> Result<Self> {
        for document in [before, after] {
            if document.metadata().id() != current.metadata().id()
                || !document.budget().same_pool(current.budget())
                || !document.disk_budget().same_pool(current.disk_budget())
            {
                return Err("Native task source document or admission differs".into());
            }
        }
        let count = before
            .core
            .roster
            .roster
            .len()
            .checked_add(after.core.roster.roster.len())
            .and_then(|n| n.checked_add(current.core.roster.roster.len()))
            .ok_or("Native task source count overflow")?;
        let bytes = count
            .checked_mul(2 * std::mem::size_of::<Surface>() + std::mem::size_of::<StagedOutput>())
            .and_then(|n| n.checked_add(4096 * std::mem::size_of::<TilePatch>() + 4096))
            .ok_or("Native task source workspace overflow")?;
        let charge = current
            .budget()
            .reserve_bytes(bytes, bytes)
            .map_err(|e| e.to_string())?;
        let mut prepared = Vec::new();
        let mut imported = Vec::new();
        let mut outputs = Vec::new();
        prepared
            .try_reserve_exact(count)
            .map_err(|_| "Native task source allocation refused")?;
        imported
            .try_reserve_exact(count)
            .map_err(|_| "Native task roster allocation refused")?;
        outputs
            .try_reserve_exact(count)
            .map_err(|_| "Native task output allocation refused")?;
        if prepared.capacity() > count || imported.capacity() > count || outputs.capacity() > count
        {
            return Err("Native task source workspace exceeds admission".into());
        }
        session.check()?;
        check()?;
        Ok(Self {
            before,
            after,
            current,
            session,
            check,
            prepared,
            imported,
            outputs,
            pending: None,
            refused: None,
            _charge: charge,
        })
    }
    fn source(&self, source: Source<'_>) -> Result<&Surface> {
        let (reference, surface) = match source {
            Source::Original(side, reference) => {
                let document = match side {
                    Side::Before => self.before,
                    Side::After => self.after,
                    Side::Current => self.current,
                };
                (reference, document.core.roster.roster.get(reference)?)
            }
            Source::Prepared(reference) => (
                reference,
                self.prepared
                    .get(
                        reference
                            .id
                            .checked_sub(1)
                            .ok_or("Missing task source ordinal")? as usize,
                    )
                    .ok_or("Missing provisional task source")?,
            ),
        };
        if (reference.width, reference.height, reference.depth)
            != (surface.width(), surface.height(), surface.depth())
            || !surface.uses_budget(self.current.budget())
            || !surface.uses_disk_budget(self.current.disk_budget())
        {
            return Err("Native task source shape or admission differs".into());
        }
        Ok(surface)
    }
    fn observe(&self) -> Result<()> {
        self.session.check()?;
        (self.check)()
    }
    pub(super) fn output_count(&self) -> usize {
        self.outputs.len()
    }
    pub(super) fn document(&mut self, metadata: Admitted<Document36>) -> Result<NativeDocument> {
        let bytes = self
            .imported
            .capacity()
            .checked_mul(std::mem::size_of::<Surface>())
            .and_then(|n| n.checked_add(128))
            .ok_or("Native task roster footprint overflow")?;
        let roster_charge = self
            .current
            .budget()
            .reserve_bytes(bytes, bytes)
            .map_err(|e| e.to_string())?;
        let roster = RasterRoster::new(std::mem::take(&mut self.imported))?;
        let pending = PendingScene::new(
            metadata,
            roster,
            self.current.budget().clone(),
            self.current.disk_budget().clone(),
        )?;
        let document = NativeDocument::from_pending(pending)?;
        drop(roster_charge);
        self.observe()?;
        Ok(document)
    }
    /// The receiving worker preadmits its output owner vector before transfer.
    pub(super) fn take_outputs(&mut self) -> Vec<StagedOutput> {
        std::mem::take(&mut self.outputs)
    }
    pub(super) fn abort(mut self) -> Result<()> {
        self.prepared.clear();
        self.imported.clear();
        let mut error = None;
        if let Some(draft) = self.refused.take() {
            if let Err(reason) = draft.abort(self.session, &|| self.session.cleanup_check()) {
                error.get_or_insert(reason);
            }
        }
        if let Err(reason) = super::super::output::abort_outputs(
            std::mem::take(&mut self.outputs),
            self.session,
            &|| self.session.cleanup_check(),
        ) {
            error.get_or_insert(reason);
        }
        if let Some(fragment) = self.pending.take() {
            if let Err(reason) = self
                .session
                .abort_fragment(fragment, &|| self.session.cleanup_check())
            {
                error.get_or_insert(reason);
            }
        }
        error.map_or(Ok(()), Err)
    }
}
fn reference(surface: &Surface, id: usize) -> Result<RasterRef36> {
    Ok(RasterRef36 {
        id: u32::try_from(id).map_err(|_| "Native task source ordinal overflow")?,
        width: surface.width(),
        height: surface.height(),
        depth: surface.depth(),
        retained: None,
    })
}
fn extend(out: &mut Option<[i32; 4]>, bounds: [u32; 4], origin: [i32; 2]) -> Result<()> {
    let mut rect = [0; 4];
    for i in 0..4 {
        rect[i] = i64::from(bounds[i])
            .checked_add(i64::from(origin[i % 2]))
            .and_then(|n| i32::try_from(n).ok())
            .ok_or("Native task changed bounds overflow")?;
    }
    *out = Some(match *out {
        Some(old) => [
            old[0].min(rect[0]),
            old[1].min(rect[1]),
            old[2].max(rect[2]),
            old[3].max(rect[3]),
        ],
        None => rect,
    });
    Ok(())
}
impl<S: StageSession> Resolver for Sources<'_, S> {
    fn same(&mut self, left: Source<'_>, right: Source<'_>) -> Result<bool> {
        self.observe()?;
        let (left, right) = (self.source(left)?, self.source(right)?);
        if (left.width(), left.height(), left.depth())
            != (right.width(), right.height(), right.depth())
        {
            return Ok(false);
        }
        let mut a = left.physical_tiles();
        let mut b = right.physical_tiles();
        loop {
            match (a.next(), b.next()) {
                (None, None) => return Ok(true),
                (Some(a), Some(b)) if a.coordinate() == b.coordinate() => {
                    self.observe()?;
                    // Equality always verifies actual padded native bytes. A
                    // missing/corrupt source is never treated as transparent.
                    let mut check = || {
                        self.observe()
                            .map_err(|_| crate::large_document::Error::Cancelled)
                    };
                    let aa = left
                        .prepare_tile(a.coordinate(), &mut check)
                        .map_err(|e| e.to_string())?;
                    let bb = right
                        .prepare_tile(b.coordinate(), &mut check)
                        .map_err(|e| e.to_string())?;
                    if aa.bytes() != bb.bytes() {
                        return Ok(false);
                    }
                }
                _ => return Ok(false),
            }
        }
    }
    fn import(&mut self, source: Source<'_>) -> Result<RasterRef36> {
        self.observe()?;
        if self.imported.len() == self.imported.capacity() {
            return Err("Native task final roster exceeds admission".into());
        }
        let surface = self.source(source)?.clone();
        for tile in surface.physical_tiles() {
            let mut check = || {
                self.observe()
                    .map_err(|_| crate::large_document::Error::Cancelled)
            };
            drop(
                surface
                    .prepare_tile(tile.coordinate(), &mut check)
                    .map_err(|e| e.to_string())?,
            );
        }
        let reference = reference(&surface, self.imported.len() + 1)?;
        self.imported.push(surface);
        Ok(reference)
    }
    fn inverse(
        &mut self,
        b: Source<'_>,
        a: Source<'_>,
        c: Source<'_>,
        _: &str,
        _: &str,
        origin: [i32; 2],
    ) -> Result<RasterOutcome> {
        use super::super::pixel_inverse::{PixelInverse, TileInverse};
        self.observe()?;
        let (before, after, current) = (
            self.source(b)?.clone(),
            self.source(a)?.clone(),
            self.source(c)?.clone(),
        );
        let kernel = PixelInverse::new(
            &before,
            &after,
            &current,
            self.current.budget(),
            self.current.disk_budget(),
        )?;
        let mut patches = Vec::new();
        patches
            .try_reserve_exact(4096)
            .map_err(|_| "Native task patch allocation refused")?;
        if patches.capacity() > 4096 {
            return Err("Native task patches exceed admission".into());
        }
        let mut changed = None;
        let mut conflicts = None;
        let spec = TileSpec::new(256, 256, current.depth()).map_err(|e| e.to_string())?;
        for coordinate in kernel.coordinates() {
            self.observe()?;
            match kernel.tile(coordinate, &|| self.observe())? {
                TileInverse::Unchanged => {}
                TileInverse::Conflict {
                    changed: r,
                    conflicts: c,
                } => {
                    extend(&mut changed, r, origin)?;
                    extend(&mut conflicts, c, origin)?;
                }
                TileInverse::Merged { pixels, changed: r } => {
                    extend(&mut changed, r, origin)?;
                    // Preserve explicit physical presence, including all-zero
                    // current blocks. Absence is valid only for a complete zero
                    // tile after its byte/padding proof.
                    let was_present = current
                        .physical_tiles()
                        .any(|t| t.coordinate() == coordinate);
                    if !was_present && pixels.iter().all(|&b| b == 0) {
                        patches.push(
                            TilePatch::zero(coordinate, spec, &pixels, self.current.budget())
                                .map_err(|e| e.to_string())?,
                        );
                    } else {
                        if self.pending.is_none() {
                            self.pending = Some(self.session.fresh_fragment()?);
                        }
                        let store = self.pending.as_ref().unwrap().store();
                        let block = store
                            .put(spec, &pixels, &mut || {
                                self.observe()
                                    .map_err(|_| crate::large_document::Error::Cancelled)
                            })
                            .map_err(|e| e.to_string())?;
                        patches.push(TilePatch::stored(coordinate, store.clone(), block));
                    }
                }
            }
        }
        let mut result = current
            .with_tile_patches(&patches, self.current.budget(), self.current.disk_budget())
            .map_err(|e| e.to_string())?;
        drop(patches);
        if conflicts.is_some() {
            // A conflict carries no publishable pixel candidate. Retain the
            // original current source; the outer worker aborts every output.
            result = current.clone();
        }
        if let Some(fragment) = self.pending.take() {
            if conflicts.is_some() {
                self.session
                    .abort_fragment(fragment, &|| self.session.cleanup_check())?;
            } else {
                let draft = OutputDraft::new(result.clone(), vec![fragment]);
                match draft.seal(self.current, self.session, &|| self.observe()) {
                    Ok(output) => {
                        if self.outputs.len() == self.outputs.capacity() {
                            self.refused = Some(output.into_draft());
                            return Err("Native task output exceeds admission".into());
                        }
                        self.outputs.push(output);
                    }
                    Err(refused) => {
                        let reason = refused.reason().to_owned();
                        self.refused = Some(refused.into_draft());
                        return Err(reason);
                    }
                }
            }
        }
        if self.prepared.len() == self.prepared.capacity() {
            return Err("Native task prepared sources exceed admission".into());
        }
        let plane = reference(&result, self.prepared.len() + 1)?;
        self.prepared.push(result);
        self.observe()?;
        Ok(RasterOutcome {
            plane,
            changed,
            conflicts,
        })
    }
}
