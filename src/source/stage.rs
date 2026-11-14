//! Worker output only. Source definition/matrix, revision, lease and source slot
//! remain guarded by the captured Native batch at final Engine adoption.
use super::prepared::PreparedSource;
use crate::{
    engine::native::{Cancellation, NativeDocument, OutputDraft, StagedOutput},
    large_document::{Error, FreshFragment, ScratchBytes, TileSpec},
    raster::{
        backed::{Plane, Region},
        Surface, TILE,
    },
    retained::transaction::Session,
};
use std::cell::RefCell;
use std::collections::BTreeMap;
#[cfg(test)]
#[path = "stage_tests.rs"]
pub(crate) mod tests;

/// Renderer output only. This private adapter cannot publish or grant source
/// authority. The common actual tile writer owns all provisional file handling.
pub(super) trait TileProducer {
    fn fill_tile(
        &mut self,
        tx: u32,
        ty: u32,
        tile: &mut [u8],
        check: &mut dyn FnMut() -> Result<(), String>,
    ) -> Result<(), String>;
    fn complete(&self) -> Result<(), String>;
}

/// SVG kernel work requires the real captured edit's pure cancellation token.
/// Session/source callbacks stay outside the synchronous renderer boundary.
pub(crate) fn render_captured(
    source: &PreparedSource<'_>,
    frame: [u32; 2],
    depth: u16,
    base: &NativeDocument,
    session: &impl Session,
    cancellation: &Cancellation,
    check: &mut dyn FnMut() -> Result<(), String>,
) -> Result<StagedOutput, String> {
    cancellation.check()?;
    if source.svg_plan().is_some() {
        return super::native_svg::render(source, frame, depth, base, session, cancellation, check);
    }
    render_inner(source, frame, depth, base, session, None, &mut || {
        cancellation.check()?;
        check()
    })
}

/// No file is published here. Seal under the same document/session while the
/// proposal index ticket remains held; only Engine may adopt an atomic batch.
pub(crate) fn render(
    source: &PreparedSource<'_>,
    frame: [u32; 2],
    depth: u16,
    base: &NativeDocument,
    session: &impl Session,
    check: &mut dyn FnMut() -> Result<(), String>,
) -> Result<StagedOutput, String> {
    if source.svg_plan().is_some() {
        return Err("SVG staging requires its captured native cancellation token".into());
    }
    render_inner(source, frame, depth, base, session, None, check)
}
pub(super) fn render_inner(
    source: &PreparedSource<'_>,
    frame: [u32; 2],
    depth: u16,
    base: &NativeDocument,
    session: &impl Session,
    mut producer: Option<&mut dyn TileProducer>,
    check: &mut dyn FnMut() -> Result<(), String>,
) -> Result<StagedOutput, String> {
    session.check()?;
    check()?;
    if source.svg_plan().is_some() != producer.is_some() {
        return Err("SVG staging must consume its captured complete-band producer".into());
    }
    let pool = session.resident();
    if !source.uses_budget(&pool)
        || !base.budget().same_pool(&pool)
        || !base.disk_budget().same_pool(&session.disk())
        || ![8, 16].contains(&depth)
        || frame.iter().any(|n| *n == 0 || *n > 16000)
    {
        return Err("Vector staging frame, precision or source admission differs".into());
    }
    let count = frame[0].div_ceil(TILE) as usize * frame[1].div_ceil(TILE) as usize;
    let index_bytes = count
        .checked_mul(512)
        .and_then(|n| n.checked_add(4096))
        .ok_or("Vector staging index overflow")?;
    let _index = pool
        .reserve_bytes(index_bytes, index_bytes)
        .map_err(|e| e.to_string())?;
    let mut fragment: Option<FreshFragment> = None;
    let result: Result<Surface, String> = (|| {
        let mut guarded = || {
            session.check()?;
            check()
        };
        if source.is_known_zero() {
            guarded()?;
            return Surface::zero(frame[0], frame[1], depth).map_err(|e| e.to_string());
        }
        let mut blocks = BTreeMap::new();
        let spec = TileSpec::new(TILE as u16, TILE as u16, depth).map_err(|e| e.to_string())?;
        let stride = if depth == 16 { 8 } else { 4 };
        let [min_x, min_y, max_x, max_y] = source.direct_shape_bounds(frame)
            .unwrap_or([0, 0, frame[0], frame[1]]);
        let end_y = if min_x < max_x && min_y < max_y {
            max_y.div_ceil(TILE)
        } else {
            min_y / TILE
        };
        for ty in min_y / TILE..end_y {
            for tx in min_x / TILE..max_x.div_ceil(TILE) {
                guarded()?;
                let mut tile = ScratchBytes::new(&pool, spec.bytes(), spec.bytes())
                    .map_err(|e| e.to_string())?;
                let left = tx * TILE;
                let top = ty * TILE;
                let right = (left + TILE).min(frame[0]);
                let bottom = (top + TILE).min(frame[1]);
                if let Some(producer) = producer.as_deref_mut() {
                    producer.fill_tile(tx, ty, &mut tile, &mut guarded)?;
                } else {
                    let [x0, y0, x1, y1] = [
                        left.max(min_x), top.max(min_y), right.min(max_x), bottom.min(max_y),
                    ];
                    let columns = (x1 - x0).min(source.region_pixels() as u32);
                    let rows = (source.region_pixels() / columns as usize).clamp(1, 32) as u32;
                    for y in (y0..y1).step_by(rows as usize) {
                        for x in (x0..x1).step_by(columns as usize) {
                            let end = (y + rows).min(y1);
                            let end_x = (x + columns).min(x1);
                            let region =
                                Region::new([x as i32, y as i32, end_x as i32, end as i32])
                                    .map_err(|e| e.to_string())?;
                            let pixels =
                                source.render_region(frame, region, depth, &pool, &mut guarded)?;
                            for (row, bytes) in pixels
                                .bytes()
                                .chunks_exact((end_x - x) as usize * stride)
                                .enumerate()
                            {
                                let at = (((y - top) as usize + row) * TILE as usize
                                    + (x - left) as usize)
                                    * stride;
                                tile[at..at + bytes.len()].copy_from_slice(bytes);
                            }
                        }
                    }
                }
                if tile.iter().all(|v| *v == 0) {
                    continue;
                }
                if fragment.is_none() {
                    fragment = Some(session.fresh()?);
                }
                let mut refused = None;
                let block = fragment
                    .as_ref()
                    .unwrap()
                    .store()
                    .put(spec, &tile, &mut || {
                        guarded().map_err(|e| {
                            refused = Some(e);
                            Error::Cancelled
                        })
                    })
                    .map_err(|e| refused.unwrap_or_else(|| e.to_string()))?;
                blocks.insert((tx, ty), block);
                #[cfg(test)]
                tests::observe_completed_tile();
            }
        }
        if let Some(producer) = producer.as_deref() {
            producer.complete()?;
        }
        guarded()?;
        let surface = if let Some(held) = fragment.as_ref() {
            Surface::from_plane(
                Plane::from_blocks(frame[0], frame[1], depth, held.store().clone(), blocks)
                    .map_err(|e| e.to_string())?,
            )
            .map_err(|e| e.to_string())?
        } else {
            Surface::zero(frame[0], frame[1], depth).map_err(|e| e.to_string())?
        };
        guarded()?;
        Ok(surface)
    })();
    match result {
        Ok(surface) => {
            let draft = OutputDraft::new(surface, fragment.into_iter().collect());
            let callback = RefCell::new(check);
            match draft.seal(base, session, &|| {
                session.check()?;
                (callback.borrow_mut())()
            }) {
                Ok(staged) => Ok(staged),
                Err(refused) => {
                    let mut error = refused.reason().to_string();
                    if let Err(cleanup) = refused.into_draft().abort(session, &|| Ok(())) {
                        error.push_str("; vector source cleanup retained inventory: ");
                        error.push_str(&cleanup);
                    }
                    Err(error)
                }
            }
        }
        Err(mut error) => {
            // All map/Surface/store clones in the closure have dropped. Ignore
            // only gesture cancellation; actual retirement admission still runs.
            if let Some(fragment) = fragment {
                if let Err(cleanup) = session.abort_fragment(fragment, &|| Ok(())) {
                    error.push_str("; vector source cleanup retained inventory: ");
                    error.push_str(&cleanup);
                }
            }
            Err(error)
        }
    }
}
