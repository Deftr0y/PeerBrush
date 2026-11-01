//! Complete physical frame remapping through an actual source session. The
//! coordinator must stage color, every mask and retained/source placements as
//! one batch; this worker cannot change layer metadata or publish a document.
use super::*;
use crate::{edit_bounds::input::InputFrame, raster::backed::Plane};

pub(crate) fn stage_frame<'d, 's>(
    input: &RasterInput<'d>,
    plan: &InputFrame,
    session: &'s SourceSession,
    check: &mut dyn FnMut() -> Result<(), String>,
) -> Result<TileDraft<'d, 's>, String> {
    stage_frame_with(input, plan, session, check, |_, bytes, _| Ok(bytes.clone()))
}
pub(crate) fn stage_frame_with<'d, 's>(
    input: &RasterInput<'d>,
    plan: &InputFrame,
    session: &'s SourceSession,
    check: &mut dyn FnMut() -> Result<(), String>,
    edit: impl FnMut(
        (u32, u32),
        &PinnedBytes,
        &mut dyn FnMut() -> Result<(), String>,
    ) -> Result<PinnedBytes, String>,
) -> Result<TileDraft<'d, 's>, String> {
    stage_frame_seed(input, plan, session, check, edit)?.finish(check)
}

pub(crate) struct FrameSeed<'d, 's> {
    base: &'d NativeDocument,
    session: &'s SourceSession,
    surface: Surface,
    fragments: Vec<FreshFragment>,
    poisoned: bool,
    _charge: Reservation,
}
impl<'d, 's> FrameSeed<'d, 's> {
    pub(crate) fn shape(&self) -> [u32; 2] {
        [self.surface.width(), self.surface.height()]
    }
    pub(crate) fn depth(&self) -> u16 {
        self.surface.depth()
    }
    pub(crate) fn prepare_tile(
        &self,
        key: (u32, u32),
        check: &mut dyn FnMut() -> Result<(), String>,
    ) -> Result<PinnedBytes, String> {
        self.ensure(check)?;
        let tile = self
            .surface
            .prepare_tile(key, &mut || {
                self.session.check().map_err(|_| Error::Changed)?;
                check().map_err(|_| Error::Cancelled)
            })
            .map_err(|e| e.to_string())?;
        let bytes = tile
            .copy_to_scratch(self.base.budget())
            .map_err(|e| e.to_string())?;
        self.ensure(check)?;
        Ok(bytes.freeze())
    }
    pub(crate) fn prepare_strip(
        &self,
        area: [i32; 4],
        check: &mut dyn FnMut() -> Result<(), String>,
    ) -> Result<PinnedBytes, String> {
        self.ensure(check)?;
        let shape = self.shape();
        if area[0] < 0 || area[1] < 0 || area[2] > shape[0] as i32 || area[3] > shape[1] as i32 {
            return Err("Native framed strip exceeds its captured output geometry".into());
        }
        let region = crate::raster::backed::Region::new(area).map_err(|e| e.to_string())?;
        let bpp = if self.depth() == 16 { 8 } else { 4 };
        let stride = (area[2] - area[0]) as usize * bpp;
        let length = region.pixels() * bpp;
        let mut bytes =
            ScratchBytes::new(self.base.budget(), length, length).map_err(|e| e.to_string())?;
        let view = self
            .surface
            .prepare_region(region, &mut || {
                self.session.check().map_err(|_| Error::Changed)?;
                check().map_err(|_| Error::Cancelled)
            })
            .map_err(|e| e.to_string())?;
        for (index, row) in bytes.chunks_exact_mut(stride).enumerate() {
            check()?;
            view.copy_row(area[1] + index as i32, row)
                .map_err(|e| e.to_string())?;
        }
        self.ensure(check)?;
        Ok(bytes.freeze())
    }
    fn ensure(&self, check: &mut dyn FnMut() -> Result<(), String>) -> Result<(), String> {
        if self.poisoned {
            return Err("Native framed working output is poisoned".into());
        }
        self.session.check()?;
        check()
    }
    pub(crate) fn replace_tile(
        &mut self,
        key: (u32, u32),
        bytes: PinnedBytes,
        check: &mut dyn FnMut() -> Result<(), String>,
    ) -> Result<(), String> {
        let result = self.replace_inner(key, bytes, check);
        if result.is_err() {
            self.poisoned = true;
        }
        result
    }
    fn replace_inner(
        &mut self,
        key: (u32, u32),
        bytes: PinnedBytes,
        check: &mut dyn FnMut() -> Result<(), String>,
    ) -> Result<(), String> {
        self.ensure(check)?;
        let spec =
            TileSpec::new(TILE as u16, TILE as u16, self.depth()).map_err(|e| e.to_string())?;
        let shape = self.shape();
        if !bytes.uses_budget(self.base.budget())
            || bytes.len() != spec.bytes()
            || key.0 >= shape[0].div_ceil(TILE)
            || key.1 >= shape[1].div_ceil(TILE)
        {
            return Err("Native framed tile differs from its admitted shape or owner".into());
        }
        let before = self.prepare_tile(key, check)?;
        let mut same = true;
        let mut zero = true;
        for (old, new) in before.chunks(64 * 1024).zip(bytes.chunks(64 * 1024)) {
            self.ensure(check)?;
            same &= old == new;
            zero &= new.iter().all(|v| *v == 0);
        }
        if same {
            return Ok(());
        }
        let patch = if zero {
            TilePatch::zero(key, spec, &bytes, self.base.budget()).map_err(|e| e.to_string())?
        } else {
            if self.fragments.is_empty() {
                if self.fragments.capacity() == 0 {
                    self.fragments
                        .try_reserve_exact(1)
                        .map_err(|_| "Native framed fragment owner allocation failed")?;
                    if self.fragments.capacity() != 1 {
                        return Err("Native framed owner exceeded admission".into());
                    }
                }
                self.fragments.push(self.session.fresh_fragment()?);
            }
            let store = self.fragments[0].store();
            let reference = store
                .put(spec, &bytes, &mut || {
                    self.session.check().map_err(|_| Error::Changed)?;
                    check().map_err(|_| Error::Cancelled)
                })
                .map_err(|e| e.to_string())?;
            TilePatch::stored(key, store.clone(), reference)
        };
        let candidate = self
            .surface
            .with_tile_patches(&[patch], self.base.budget(), self.base.disk_budget())
            .map_err(|e| e.to_string())?;
        self.ensure(check)?;
        self.surface = candidate;
        Ok(())
    }
    pub(crate) fn abort(self) -> Result<(), String> {
        let Self {
            surface,
            fragments,
            session,
            _charge,
            ..
        } = self;
        drop(surface);
        let mut error = None;
        for fragment in fragments {
            if let Err(reason) = session.abort_fragment(fragment, &mut || Ok(())) {
                error.get_or_insert(reason);
            }
        }
        drop(_charge);
        error.map_or(Ok(()), Err)
    }
    pub(crate) fn finish(
        mut self,
        check: &mut dyn FnMut() -> Result<(), String>,
    ) -> Result<TileDraft<'d, 's>, String> {
        let result = (|| {
            self.ensure(check)?;
            if self.surface.physical_tiles().next().is_none() {
                let shape = self.shape();
                self.surface =
                    Surface::zero(shape[0], shape[1], self.depth()).map_err(|e| e.to_string())?;
            }
            let mut index = 0;
            while index < self.fragments.len() {
                self.ensure(check)?;
                let identity = self.fragments[index].store().identity();
                if self
                    .surface
                    .physical_tiles()
                    .any(|tile| tile.owner().identity() == identity)
                {
                    index += 1;
                } else {
                    self.session
                        .abort_fragment(self.fragments.remove(index), &mut || Ok(()))?;
                }
            }
            self.ensure(check)
        })();
        if let Err(mut reason) = result {
            if let Err(cleanup) = self.abort() {
                reason.push_str("; native framed cleanup retained inventory: ");
                reason.push_str(&cleanup);
            }
            return Err(reason);
        }
        let Self {
            surface,
            fragments,
            base,
            session,
            _charge,
            ..
        } = self;
        Ok(TileDraft {
            draft: OutputDraft::new(surface, fragments),
            base,
            session,
            _charge,
        })
    }
}

pub(crate) fn stage_frame_seed<'d, 's>(
    input: &RasterInput<'d>,
    plan: &InputFrame,
    session: &'s SourceSession,
    check: &mut dyn FnMut() -> Result<(), String>,
    mut edit: impl FnMut(
        (u32, u32),
        &PinnedBytes,
        &mut dyn FnMut() -> Result<(), String>,
    ) -> Result<PinnedBytes, String>,
) -> Result<FrameSeed<'d, 's>, String> {
    input.validate(plan.binding())?;
    session.check()?;
    check()?;
    let base = input.document();
    if base.metadata().read_only()
        || !base.budget().same_pool(&session.resident())
        || !base.disk_budget().same_pool(&session.disk())
    {
        return Err("Native frame output requires its actual source session".into());
    }
    let geometry = plan.geometry();
    if geometry.old_shape() != input.shape() || geometry.old_origin() != input.origin() {
        return Err("Native frame geometry differs from its immutable input".into());
    }
    let shape = geometry.new_shape();
    let count = shape[0].div_ceil(TILE) as usize * shape[1].div_ceil(TILE) as usize;
    let admitted = count
        .checked_mul(512)
        .and_then(|n| n.checked_add(4096))
        .ok_or("Native frame index admission overflow")?;
    let charge = base
        .budget()
        .reserve_bytes(admitted, admitted)
        .map_err(|e| e.to_string())?;
    let mut fragment = None;
    let result: Result<Surface, String> = (|| {
        let mut blocks = BTreeMap::new();
        let mut guarded = || {
            session.check()?;
            check()
        };
        let spec =
            TileSpec::new(TILE as u16, TILE as u16, input.depth()).map_err(|e| e.to_string())?;
        for key in geometry.tiles() {
            guarded()?;
            let prepared = geometry
                .source_reads(key)?
                .map(|r| input.prepare_region(r, &mut guarded))
                .transpose()?;
            let bytes = geometry.render_tile(key, &mut guarded, |x, y| {
                prepared
                    .as_ref()
                    .ok_or_else(|| {
                        "Frame sampler missed its declared old-source footprint".to_owned()
                    })?
                    .pixel16(x, y)
            })?;
            let bytes = edit(key, &bytes, &mut guarded)?;
            if !bytes.uses_budget(base.budget()) || bytes.len() != spec.bytes() {
                return Err("Edited frame tile differs from its admitted native byte owner".into());
            }
            let mut zero = true;
            for part in bytes.chunks(64 * 1024) {
                guarded()?;
                zero &= part.iter().all(|b| *b == 0);
            }
            if zero {
                continue;
            }
            if fragment.is_none() {
                fragment = Some(session.fresh_fragment()?);
            }
            let store = fragment.as_ref().unwrap().store();
            let reference = store
                .put(spec, &bytes, &mut || {
                    guarded().map_err(|_| Error::Cancelled)
                })
                .map_err(|e| e.to_string())?;
            blocks.insert(key, reference);
        }
        guarded()?;
        if blocks.is_empty() {
            return Surface::zero(shape[0], shape[1], input.depth()).map_err(|e| e.to_string());
        }
        let store = fragment
            .as_ref()
            .ok_or("Native frame fragment is missing")?
            .store()
            .clone();
        let plane = Plane::from_blocks(shape[0], shape[1], input.depth(), store, blocks)
            .map_err(|e| e.to_string())?;
        // The provisional map admission remains alive until the permanent
        // Surface index charge has been acquired successfully.
        Surface::from_plane(plane).map_err(|e| e.to_string())
    })();
    let surface = match result {
        Ok(surface) => surface,
        Err(mut error) => {
            if let Some(fragment) = fragment {
                if let Err(cleanup) = session.abort_fragment(fragment, &mut || Ok(())) {
                    error.push_str("; native frame cleanup retained inventory: ");
                    error.push_str(&cleanup);
                }
            }
            return Err(error);
        }
    };
    let mut fragments = Vec::new();
    if fragment.is_some() && (fragments.try_reserve_exact(1).is_err() || fragments.capacity() != 1)
    {
        drop(surface);
        let cleanup = session.abort_fragment(fragment.take().unwrap(), &mut || Ok(()));
        return Err(match cleanup {
            Ok(()) => "Native frame owner allocation failed".into(),
            Err(e) => {
                format!("Native frame owner allocation failed; cleanup retained inventory: {e}")
            }
        });
    }
    if let Some(fragment) = fragment {
        fragments.push(fragment);
    }
    Ok(FrameSeed {
        surface,
        fragments,
        poisoned: false,
        base,
        session,
        _charge: charge,
    })
}
