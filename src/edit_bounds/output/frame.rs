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
    mut edit: impl FnMut(
        (u32, u32),
        &PinnedBytes,
        &mut dyn FnMut() -> Result<(), String>,
    ) -> Result<PinnedBytes, String>,
) -> Result<TileDraft<'d, 's>, String> {
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
    Ok(TileDraft {
        draft: OutputDraft::new(surface, fragments),
        base,
        session,
        _charge: charge,
    })
}
