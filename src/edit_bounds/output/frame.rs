//! Complete physical frame remapping through an actual source session. The
//! coordinator must stage color, every mask and retained/source placements as
//! one batch; this worker cannot change layer metadata or publish a document.
use super::*;
use crate::edit_bounds::{input::InputFrame, mask_original::MaskOriginal};

pub(crate) fn stage_frame<'d, 's>(
    input: &RasterInput<'d>,
    plan: &InputFrame,
    session: &'s SourceSession,
    check: &mut dyn FnMut() -> Result<(), String>,
) -> Result<TileDraft<'d, 's>, String> {
    stage_frame_seed(input, plan, session, check)?.finish(check)
}
pub(crate) fn stage_frame_with<'d, 's>(
    input: &RasterInput<'d>,
    plan: &InputFrame,
    session: &'s SourceSession,
    check: &mut dyn FnMut() -> Result<(), String>,
    // A bounded footprint is in row-major order. None allows edits on every tile.
    edited_tiles: Option<&[(u32, u32)]>,
    edit: impl FnMut(
        (u32, u32),
        &PinnedBytes,
        &mut dyn FnMut() -> Result<(), String>,
    ) -> Result<PinnedBytes, String>,
) -> Result<TileDraft<'d, 's>, String> {
    build_frame(input, plan, session, check, edited_tiles, edit)?.finish(check)
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
    /// A genuinely new plane has an explicit zero canvas, not a fabricated
    /// existing RasterSlot. Only the engine's typed insertion can adopt it.
    pub(crate) fn empty_canvas(
        base: &'d NativeDocument,
        session: &'s SourceSession,
        check: &mut dyn FnMut() -> Result<(), String>,
    ) -> Result<Self, String> {
        session.check()?;
        check()?;
        if base.metadata().read_only()
            || !base.budget().same_pool(&session.resident())
            || !base.disk_budget().same_pool(&session.disk())
        {
            return Err("New native canvas requires its editable captured source session".into());
        }
        let metadata = base.metadata();
        let surface = Surface::zero(metadata.width(), metadata.height(), metadata.depth())
            .map_err(|e| e.to_string())?;
        let count =
            metadata.width().div_ceil(TILE) as usize * metadata.height().div_ceil(TILE) as usize;
        let bytes = count
            .checked_mul(512)
            .and_then(|n| n.checked_add(4096))
            .ok_or("New native canvas index admission overflow")?;
        let charge = base
            .budget()
            .reserve_bytes(bytes, bytes)
            .map_err(|e| e.to_string())?;
        check()?;
        Ok(Self {
            base,
            session,
            surface,
            fragments: Vec::new(),
            poisoned: false,
            _charge: charge,
        })
    }
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
) -> Result<FrameSeed<'d, 's>, String> {
    build_frame(input, plan, session, check, Some(&[]), |_, bytes, _| {
        Ok(bytes.clone())
    })
}

fn tile_is_zero(
    bytes: &[u8],
    check: &mut dyn FnMut() -> Result<(), String>,
) -> Result<bool, String> {
    let mut zero = true;
    for part in bytes.chunks(64 * 1024) {
        check()?;
        zero &= part.iter().all(|b| *b == 0);
    }
    Ok(zero)
}

pub(crate) fn prepare_frame_tile(
    input: &RasterInput<'_>,
    plan: &InputFrame,
    key: (u32, u32),
    check: &mut dyn FnMut() -> Result<(), String>,
) -> Result<PinnedBytes, String> {
    input.validate(plan.binding())?;
    let original = MaskOriginal::capture(input)?;
    let revealed = revealed_tile(plan, key, original.as_ref())?;
    let reads = plan.geometry().source_reads(key)?;
    render_frame_tile(input, plan, key, reads, revealed, check)
}

fn revealed_tile<'a, 'd>(
    plan: &InputFrame,
    key: (u32, u32),
    original: Option<&'a MaskOriginal<'d>>,
) -> Result<Option<(&'a MaskOriginal<'d>, [i32; 4])>, String> {
    let Some(original) = original else { return Ok(None); };
    let area = plan.tile_document_rect(key)?;
    Ok(original.reveals(area).then_some((original, area)))
}

fn render_frame_tile(
    input: &RasterInput<'_>,
    plan: &InputFrame,
    key: (u32, u32),
    reads: Option<[i32; 4]>,
    revealed: Option<(&MaskOriginal<'_>, [i32; 4])>,
    check: &mut dyn FnMut() -> Result<(), String>,
) -> Result<PinnedBytes, String> {
    let stride = usize::from(input.depth() / 8) * 4;
    let length = (TILE * TILE) as usize * stride;
    if let Some((original, area)) = revealed {
        let projected = original.prepare(area, &mut || check().map_err(|_| Error::Cancelled))?;
        let mut bytes = ScratchBytes::new(input.pool(), length, length).map_err(|e| e.to_string())?;
        let row_bytes = (area[2] - area[0]) as usize * stride;
        for (y, row) in (area[1]..area[3]).zip(bytes.chunks_exact_mut(TILE as usize * stride)) {
            check()?;
            projected.copy_row(y, &mut row[..row_bytes])?;
        }
        return Ok(bytes.freeze());
    }
    if let Some(rect) = reads {
        let prepared = input.prepare_region(rect, check)?;
        plan.geometry().render_tile(key, check, |y, row| prepared.copy_row(y, row))
    } else {
        ScratchBytes::new(input.pool(), length, length)
            .map(ScratchBytes::freeze)
            .map_err(|e| e.to_string())
    }
}

fn build_frame<'d, 's>(
    input: &RasterInput<'d>,
    plan: &InputFrame,
    session: &'s SourceSession,
    check: &mut dyn FnMut() -> Result<(), String>,
    edited_tiles: Option<&[(u32, u32)]>,
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
    if edited_tiles.is_some_and(|tiles| {
        tiles
            .iter()
            .any(|&(x, y)| x >= shape[0].div_ceil(TILE) || y >= shape[1].div_ceil(TILE))
            || tiles.windows(2).any(|pair| {
                let [(x0, y0), (x1, y1)] = [pair[0], pair[1]];
                (y0, x0) >= (y1, x1)
            })
    }) {
        return Err("Native frame edits have an invalid tile footprint".into());
    }
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
        let mut patches = Vec::new();
        patches
            .try_reserve_exact(count)
            .map_err(|_| "Native frame tile patch allocation failed")?;
        if patches.capacity() > count {
            return Err("Native frame tile patches exceed admission".into());
        }
        let mut guarded = || {
            session.check()?;
            check()
        };
        let spec =
            TileSpec::new(TILE as u16, TILE as u16, input.depth()).map_err(|e| e.to_string())?;
        let source = input.surface();
        let original = MaskOriginal::capture(input)?;
        if let Some(original) = &original {
            original.admit_frame(geometry.new_origin(), shape)?;
        }
        for key in geometry.tiles() {
            let edited = edited_tiles.is_none_or(|tiles| {
                tiles
                    .binary_search_by_key(&(key.1, key.0), |&(x, y)| (y, x))
                    .is_ok()
            });
            let reads = geometry
                .source_reads(key)?
                .filter(|&rect| source.has_physical_tiles(rect));
            let revealed = revealed_tile(plan, key, original.as_ref())?;
            if !edited && reads.is_none() && revealed.is_none() {
                continue;
            }
            guarded()?;
            if let Some([l, t, r, b]) = reads.filter(|_| !edited && revealed.is_none()) {
                let edge = TILE as i32;
                // Former edge padding must never become visible after a reframe.
                if l % edge == 0 && t % edge == 0 && r - l == edge && b - t == edge {
                    let coordinate = (l as u32 / TILE, t as u32 / TILE);
                    let tile = source
                        .physical_tile(coordinate)
                        .ok_or("Missing native frame source tile")?;
                    let pinned = source
                        .prepare_tile(coordinate, &mut || guarded().map_err(|_| Error::Cancelled))
                        .map_err(|e| e.to_string())?;
                    let bytes = pinned.bytes().ok_or("Missing native frame source bytes")?;
                    if !tile_is_zero(bytes, &mut guarded)? {
                        patches.push(TilePatch::stored(
                            key,
                            tile.owner().clone(),
                            tile.reference(),
                        ));
                    }
                    continue;
                }
            }
            let bytes = render_frame_tile(input, plan, key, reads, revealed, &mut guarded)?;
            let bytes = if edited {
                edit(key, &bytes, &mut guarded)?
            } else {
                bytes
            };
            if !bytes.uses_budget(base.budget()) || bytes.len() != spec.bytes() {
                return Err("Edited frame tile differs from its admitted native byte owner".into());
            }
            if tile_is_zero(&bytes, &mut guarded)? {
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
            patches.push(TilePatch::stored(key, store.clone(), reference));
        }
        guarded()?;
        // The provisional patch admission remains alive until the permanent
        // Surface index charge has been acquired successfully.
        Surface::zero(shape[0], shape[1], input.depth())
            .and_then(|surface| {
                surface.with_tile_patches(&patches, base.budget(), base.disk_budget())
            })
            .map_err(|e| e.to_string())
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
