//! Worker output only. Source definition/matrix, revision, lease and source slot
//! remain guarded by the captured Native batch at final Engine adoption.
use super::prepared::PreparedSource;
use crate::{
    engine::native::{NativeDocument, OutputDraft, StagedOutput},
    large_document::{Error, FreshFragment, ScratchBytes, TileSpec},
    raster::{
        backed::{Plane, Region},
        Surface, TILE,
    },
    workspace::source_runtime::SourceSession,
};
use std::cell::RefCell;
use std::collections::BTreeMap;
#[cfg(test)]
#[path = "stage_tests.rs"]
mod tests;

/// No file is published here. Seal under the same document/session while the
/// proposal index ticket remains held; only Engine may adopt an atomic batch.
pub(crate) fn render(
    source: &PreparedSource<'_>,
    frame: [u32; 2],
    depth: u16,
    base: &NativeDocument,
    session: &SourceSession,
    check: &mut dyn FnMut() -> Result<(), String>,
) -> Result<StagedOutput, String> {
    session.check()?;
    check()?;
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
        for ty in 0..frame[1].div_ceil(TILE) {
            for tx in 0..frame[0].div_ceil(TILE) {
                guarded()?;
                let mut tile = ScratchBytes::new(&pool, spec.bytes(), spec.bytes())
                    .map_err(|e| e.to_string())?;
                let left = tx * TILE;
                let top = ty * TILE;
                let right = (left + TILE).min(frame[0]);
                let bottom = (top + TILE).min(frame[1]);
                let columns = (right - left).min(source.region_pixels() as u32);
                let rows = (source.region_pixels() / columns as usize).clamp(1, 32) as u32;
                for y in (top..bottom).step_by(rows as usize) {
                    for x in (left..right).step_by(columns as usize) {
                        let end = (y + rows).min(bottom);
                        let end_x = (x + columns).min(right);
                        let region = Region::new([x as i32, y as i32, end_x as i32, end as i32])
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
                if tile.iter().all(|v| *v == 0) {
                    continue;
                }
                if fragment.is_none() {
                    fragment = Some(session.fresh_fragment()?);
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
                if let Err(cleanup) = session.abort_fragment(fragment, &mut || Ok(())) {
                    error.push_str("; vector source cleanup retained inventory: ");
                    error.push_str(&cleanup);
                }
            }
            Err(error)
        }
    }
}
