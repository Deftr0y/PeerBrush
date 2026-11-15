//! Integer reframing copies current samples. It must not rerender a retained
//! original, whose off-frame data has a separate metadata lifetime.
use super::render::Written;
use crate::{
    large_document::{ScratchBytes, Store, TileSpec},
    raster::{
        backed::{Plane, Region},
        Pixel, Surface, TILE,
    },
    transform::native::{write_plane_guarded, Checks, Mapping, Work},
};
use std::collections::BTreeMap;

pub(crate) fn shift(
    source: &Surface,
    frame: [u32; 2],
    offset: [i32; 2],
    fill: Option<Pixel>,
    store: &Store,
    work: &mut Work,
    checks: Checks<'_>,
) -> Result<Written, String> {
    (checks.owner)().map_err(|e| e.to_string())?;
    let pool = store.resident_budget();
    let disk = store
        .disk_budget()
        .ok_or("Current reframing needs shared disk admission")?;
    if frame.iter().any(|v| *v == 0 || *v > 16000)
        || offset.iter().any(|v| v.unsigned_abs() > 100000)
        || !source.uses_budget(&pool)
        || !source.uses_disk_budget(&disk)
    {
        return Err("Current reframe source or admission differs".into());
    }
    let Some(fill) = fill else {
        let matrix = [1., 0., 0., 1., f64::from(offset[0]), f64::from(offset[1])];
        let mapping = Mapping::retained(
            source.width(),
            source.height(),
            source.depth(),
            matrix,
            None,
            &[],
            &pool,
        )?;
        return Ok(Written {
            surface: write_plane_guarded(
                source, &mapping, frame, store, work, checks.active, checks.owner,
            )?,
            cleared_retained: false,
        });
    };
    if offset[0] < 0
        || offset[1] < 0
        || i64::from(offset[0]) + i64::from(source.width()) > i64::from(frame[0])
        || i64::from(offset[1]) + i64::from(source.height()) > i64::from(frame[1])
    {
        return Err("Fill materialization must preserve its complete logical frame".into());
    }
    work.checked_total(u64::from(frame[0]) * u64::from(frame[1]))?;
    let count = frame[0].div_ceil(TILE) as usize * frame[1].div_ceil(TILE) as usize;
    let bytes = count * 512 + 4096;
    let index = pool
        .reserve_bytes(bytes, bytes)
        .map_err(|e| e.to_string())?;
    let mut blocks = BTreeMap::new();
    let spec =
        TileSpec::new(TILE as u16, TILE as u16, source.depth()).map_err(|e| e.to_string())?;
    let stride = usize::from(source.depth() / 8) * 4;
    let same = frame == [source.width(), source.height()] && offset == [0, 0];
    let fill = fill.map(|v| u16::from(v) * 257);
    let mut changed = false;
    for ty in 0..frame[1].div_ceil(TILE) {
        for tx in 0..frame[0].div_ceil(TILE) {
            (checks.active)().map_err(|e| e.to_string())?;
            let initial = if same {
                Some(
                    source
                        .prepare_tile((tx, ty), &mut || (checks.owner)())
                        .map_err(|e| e.to_string())?,
                )
            } else {
                None
            };
            let preserve = initial.as_ref().is_some_and(|p| !p.is_known_zero());
            let mut output = if let Some(tile) = &initial {
                tile.copy_to_scratch(&pool).map_err(|e| e.to_string())?
            } else {
                ScratchBytes::new(&pool, spec.bytes(), spec.bytes()).map_err(|e| e.to_string())?
            };
            let tile = [
                (tx * TILE) as i32,
                (ty * TILE) as i32,
                (tx * TILE + TILE).min(frame[0]) as i32,
                (ty * TILE + TILE).min(frame[1]) as i32,
            ];
            let r = [
                (tile[0] - offset[0]).max(0),
                (tile[1] - offset[1]).max(0),
                (tile[2] - offset[0]).min(source.width() as i32),
                (tile[3] - offset[1]).min(source.height() as i32),
            ];
            let view = if r[0] < r[2] && r[1] < r[3] {
                Some(
                    source
                        .prepare_region(Region::new(r).map_err(|e| e.to_string())?, &mut || (checks.owner)())
                        .map_err(|e| e.to_string())?,
                )
            } else {
                None
            };
            for y in tile[1]..tile[3] {
                (checks.active)().map_err(|e| e.to_string())?;
                for x in tile[0]..tile[2] {
                    work.claim(1)?;
                    let (sx, sy) = (x - offset[0], y - offset[1]);
                    let pixel = if sx >= 0
                        && sy >= 0
                        && sx < source.width() as i32
                        && sy < source.height() as i32
                    {
                        let before = view
                            .as_ref()
                            .ok_or("Missing current reframe footprint")?
                            .pixel16(sx, sy)
                            .map_err(|e| e.to_string())?;
                        changed |= fill != before;
                        fill
                    } else {
                        [0; 4]
                    };
                    let at =
                        ((y - tile[1]) as usize * TILE as usize + (x - tile[0]) as usize) * stride;
                    if source.depth() == 16 {
                        for (pair, v) in output[at..at + 8].chunks_exact_mut(2).zip(pixel) {
                            pair.copy_from_slice(&v.to_be_bytes());
                        }
                    } else {
                        for (out, v) in output[at..at + 4].iter_mut().zip(pixel) {
                            *out = crate::raster::project16(v);
                        }
                    }
                }
            }
            if preserve || output.iter().any(|v| *v != 0) {
                blocks.insert(
                    (tx, ty),
                    store
                        .put_guarded(
                            spec,
                            &output,
                            &mut || (checks.active)(),
                            &mut || (checks.owner)(),
                        )
                        .map_err(|e| e.to_string())?,
                );
            }
        }
    }
    let surface = if blocks.is_empty() {
        drop(blocks);
        Surface::zero(frame[0], frame[1], source.depth()).map_err(|e| e.to_string())?
    } else {
        Surface::from_plane(
            Plane::from_blocks(frame[0], frame[1], source.depth(), store.clone(), blocks)
                .map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())?
    };
    drop(index);
    (checks.owner)().map_err(|e| e.to_string())?;
    Ok(Written {
        surface,
        cleared_retained: changed,
    })
}
