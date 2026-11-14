//! Keep the original raster of an off-frame paint mask coherent
//! with its current projection after a guarded pixel edit.
use super::*;
use crate::{
    edit_bounds::{
        input::RasterInput,
        mask_original::{bounds, contains, MaskOriginal},
    },
    large_document::{Error, ScratchBytes, Store},
    raster::{backed::Region, surface::TilePatch, CanvasLimits, Surface, TILE},
    transform::native::{selected::shift, write_plane, Checks, Work},
};

pub(super) fn update(
    input: &RasterInput<'_>,
    current: &Surface,
    current_origin: [i32; 2],
    writes: [i32; 4],
    session: &SourceSession,
    check: &dyn Fn() -> Result<()>,
) -> Result<Option<RetainedUpdate>> {
    let Some(original) = MaskOriginal::capture(input)? else {
        return Ok(None);
    };
    let base = input.document();
    let source = original.source;
    if current.depth() != input.depth() {
        return Err("Retained mask update differs from the captured channel depth".into());
    }
    let [x, y] = current_origin;
    let current_bounds = bounds(current_origin, [current.width(), current.height()]);
    if !contains(current_bounds, writes) {
        return Err("Native mask edit exceeds its projected frame".into());
    }
    let writes = if original.transformed() {
        current_bounds
    } else {
        writes
    };
    let bounds = crate::engine::reframe_scope::union(original.bounds(), writes);
    let origin_after = [bounds[0], bounds[1]];
    let shape = [
        (bounds[2] - bounds[0]) as u32,
        (bounds[3] - bounds[1]) as u32,
    ];
    CanvasLimits::Native
        .check(shape[0], shape[1])
        .map_err(|_| "Mask edit would exceed the retained native frame; current work preserved")?;
    original.admit_projection(shape)?;
    let shift_by = [
        original.origin[0] - origin_after[0],
        original.origin[1] - origin_after[1],
    ];
    let to_current = [origin_after[0] - x, origin_after[1] - y];
    let writes = [
        writes[0] - bounds[0],
        writes[1] - bounds[1],
        writes[2] - bounds[0],
        writes[3] - bounds[1],
    ];
    let source = crate::retained::transaction::stage(base, session, check, |store, active| {
        let baseline = if original.transformed() {
            let mapping = original.mapping(origin_after)?;
            write_plane(
                source,
                &mapping,
                shape,
                store,
                &mut Work::new(128_000_000)?,
                active,
            )?
        } else if shift_by == [0, 0] && shape == [source.width(), source.height()] {
            source.clone()
        } else {
            let mut work = Work::new(128_000_000)?;
            shift(
                source, shape, shift_by, None, store, &mut work,
                Checks { active, owner: active },
            )?
            .surface
        };
        overlay(
            &baseline,
            (!original.transformed()).then_some(&original),
            (current, to_current),
            writes,
            origin_after,
            store,
            active,
        )
    })?;
    Ok(Some(RetainedUpdate::ReplaceOutput {
        source,
        matrix: [
            1.,
            0.,
            0.,
            1.,
            f64::from(to_current[0]),
            f64::from(to_current[1]),
        ],
        projective: None,
        warps: vec![],
    }))
}

fn overlay(
    baseline: &Surface,
    previous: Option<&MaskOriginal<'_>>,
    current: (&Surface, [i32; 2]),
    writes: [i32; 4],
    original_origin: [i32; 2],
    store: &Store,
    check: &dyn Fn() -> std::result::Result<(), Error>,
) -> Result<Surface> {
    let pool = store.resident_budget();
    let disk = store
        .disk_budget()
        .ok_or("Missing mask source disk admission")?;
    let [left, top, right, bottom] = writes.map(|value| value as u32);
    let columns = left / TILE..right.div_ceil(TILE);
    let rows = top / TILE..bottom.div_ceil(TILE);
    let count = columns.len() * rows.len();
    let bytes = count * std::mem::size_of::<TilePatch>() + 128;
    let _index = pool
        .reserve_bytes(bytes, bytes)
        .map_err(|e| e.to_string())?;
    let mut patches = Vec::new();
    patches
        .try_reserve_exact(count)
        .map_err(|_| "Native mask update index refused")?;
    if patches.capacity() > count {
        return Err("Native mask update index exceeds admission".into());
    }
    let stride = usize::from(current.0.depth() / 8) * 4;
    let row_bytes = TILE as usize * stride;
    let mut before_row =
        ScratchBytes::new(&pool, row_bytes, row_bytes).map_err(|e| e.to_string())?;
    let mut after_row =
        ScratchBytes::new(&pool, row_bytes, row_bytes).map_err(|e| e.to_string())?;
    for tx in columns {
        for ty in rows.clone() {
            check().map_err(|e| e.to_string())?;
            let [x, y] = [tx * TILE, ty * TILE];
            let [l, t, r, b] = [
                left.max(x),
                top.max(y),
                right.min(x + TILE),
                bottom.min(y + TILE),
            ];
            let [ox, oy] = original_origin;
            let before = previous
                .map(|source| {
                    source.prepare(
                        [l as i32 + ox, t as i32 + oy, r as i32 + ox, b as i32 + oy],
                        &mut || check(),
                    )
                })
                .transpose()?;
            let [dx, dy] = current.1;
            let region = Region::new([l as i32 + dx, t as i32 + dy, r as i32 + dx, b as i32 + dy])
                .map_err(|e| e.to_string())?;
            let after = current
                .0
                .prepare_region(region, &mut || check())
                .map_err(|e| e.to_string())?;
            let key = (tx, ty);
            let tile = baseline
                .prepare_tile(key, &mut || check())
                .map_err(|e| e.to_string())?;
            let mut output = tile.copy_to_scratch(&pool).map_err(|e| e.to_string())?;
            for row in t..b {
                let at = ((row - y) * TILE + l - x) as usize * stride;
                let bytes = (r - l) as usize * stride;
                let target = &mut output[at..at + bytes];
                if let Some(before) = &before {
                    before.copy_row(row as i32 + oy, &mut before_row[..bytes])?;
                    after
                        .copy_row(row as i32 + dy, &mut after_row[..bytes])
                        .map_err(|e| e.to_string())?;
                    for ((old, new), pixel) in before_row[..bytes]
                        .chunks_exact(stride)
                        .zip(after_row[..bytes].chunks_exact(stride))
                        .zip(target.chunks_exact_mut(stride))
                    {
                        if old != new {
                            pixel.copy_from_slice(new);
                        }
                    }
                } else {
                    after
                        .copy_row(row as i32 + dy, target)
                        .map_err(|e| e.to_string())?;
                }
            }
            if output.iter().all(|&byte| byte == 0) {
                if !tile.is_known_zero() {
                    patches.push(
                        TilePatch::zero(key, tile.spec(), &output.freeze(), &pool)
                            .map_err(|e| e.to_string())?,
                    );
                }
            } else if tile.bytes() != Some(&output[..]) {
                let reference = store
                    .put(tile.spec(), &output, &mut || check())
                    .map_err(|e| e.to_string())?;
                patches.push(TilePatch::stored(key, store.clone(), reference));
            }
        }
    }
    baseline
        .with_tile_patches(&patches, &pool, &disk)
        .map_err(|e| e.to_string())
}
