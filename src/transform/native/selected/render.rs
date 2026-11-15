use super::{
    geometry::{Placement, Sampler},
    input::{Input, Selection},
};
use crate::{
    large_document::{Error, ScratchBytes, Store, TileSpec},
    raster::{
        backed::{Plane, Region, MAX_REGION_PIXELS},
        surface::TilePatch,
        Pixel, Pixel16, Surface, TILE,
    },
    transform::native::{Checks, Work},
};
use std::collections::BTreeMap;

const MAPPED_BLOCK: usize = 32;

pub(crate) struct Written {
    pub surface: Surface,
    pub cleared_retained: bool,
}
#[allow(clippy::too_many_arguments)]
pub(crate) fn write(
    source: &Surface,
    selection: &Selection<'_>,
    placement: &Placement,
    origin: [i32; 2],
    layer_frame: [u32; 2],
    fill: Option<Pixel>,
    reuse_base_tiles: bool,
    store: &Store,
    work: &mut Work,
    checks: Checks<'_>,
) -> Result<Written, String> {
    (checks.owner)().map_err(|e| e.to_string())?;
    if placement.area != selection.bounds
        || !source.uses_budget(selection.document().budget())
        || !source.uses_disk_budget(selection.document().disk_budget())
        || !store
            .resident_budget()
            .same_pool(selection.document().budget())
        || layer_frame.iter().any(|n| *n == 0 || *n > 16000)
        || store
            .disk_budget()
            .is_none_or(|d| !d.same_pool(selection.document().disk_budget()))
    {
        return Err("Selected transform source or admission differs".into());
    }
    if placement.noop {
        return Ok(Written {
            surface: source.clone(),
            cleared_retained: false,
        });
    }
    let [l, t, r, b] = placement.bounds;
    let next = [origin[0].min(l), origin[1].min(t)];
    let frame = [
        (origin[0] + layer_frame[0] as i32).max(r) - next[0],
        (origin[1] + layer_frame[1] as i32).max(b) - next[1],
    ];
    if frame.iter().any(|n| *n <= 0 || *n > 16000) {
        return Err("Selected transform exceeds native frame admission".into());
    }
    let frame = frame.map(|n| n as u32);
    let pool = selection.document().budget();
    let count = frame[0].div_ceil(TILE) as usize * frame[1].div_ceil(TILE) as usize;
    let index = pool
        .reserve_bytes(
            count
                .checked_mul(512)
                .and_then(|n| n.checked_add(4096))
                .ok_or("Selected output index overflow")?,
            count * 512 + 4096,
        )
        .map_err(|e| e.to_string())?;
    let mut blocks = BTreeMap::new();
    let sampler = placement.sampler(origin, false)?;
    let spec =
        TileSpec::new(TILE as u16, TILE as u16, source.depth()).map_err(|e| e.to_string())?;
    let same = origin == next && frame == [source.width(), source.height()];
    let patch_only = reuse_base_tiles && same && fill.is_none();
    let sparse_reframe = !same && fill.is_none();
    let patch_base = if patch_only {
        Some(source.clone())
    } else if sparse_reframe && reuse_base_tiles {
        Some(Surface::zero(frame[0], frame[1], source.depth()).map_err(|e| e.to_string())?)
    } else {
        None
    };
    let mut patches = Vec::new();
    if patch_base.is_some() {
        patches
            .try_reserve_exact(count)
            .map_err(|_| "Selected tile patch allocation refused")?;
        if patches.capacity() > count {
            return Err("Selected tile patch allocation exceeds admission".into());
        }
    }
    if !patch_only && !sparse_reframe {
        work.checked_total(u64::from(frame[0]) * u64::from(frame[1]))?;
    }
    let mut changed = !same;
    let affine = sampler.is_affine();
    let mut positions = if affine {
        None
    } else {
        let position_bytes = MAPPED_BLOCK * MAPPED_BLOCK * 8;
        Some(ScratchBytes::new(pool, position_bytes, position_bytes).map_err(|e| e.to_string())?)
    };
    let block = if affine { TILE as usize } else { MAPPED_BLOCK };
    let selected_source = [
        placement.area[0] - origin[0], placement.area[1] - origin[1],
        placement.area[2] - origin[0], placement.area[3] - origin[1],
    ];
    for tx in 0..frame[0].div_ceil(TILE) {
        for ty in 0..frame[1].div_ceil(TILE) {
            (checks.active)().map_err(|e| e.to_string())?;
            let tile = [
                (tx * TILE) as i32 + next[0],
                (ty * TILE) as i32 + next[1],
                ((tx * TILE + TILE).min(frame[0])) as i32 + next[0],
                ((ty * TILE + TILE).min(frame[1])) as i32 + next[1],
            ];
            let mut bounds = intersect(tile, placement.bounds);
            if fill.is_none() && !empty(bounds) {
                if !affine {
                    work.claim(4 * sampler.sample_cost())?;
                }
                if let Some(footprint) = sampler.source_footprint(bounds) {
                    let footprint = intersect(footprint, selected_source);
                    let [left, top, right, bottom] = footprint;
                    let document_footprint = [
                        left + origin[0],
                        top + origin[1],
                        right + origin[0],
                        bottom + origin[1],
                    ];
                    if !occupied_source(source, footprint, work)?
                        || !coverage_occupied(selection, document_footprint, work)?
                    {
                        bounds = [0; 4];
                    }
                }
            }
            // A hole in raw coverage leaves this part of the source intact.
            let mut cut = intersect(tile, placement.area);
            if !empty(cut) && !coverage_occupied(selection, cut, work)? {
                cut = [0; 4];
            }
            let local = [
                tile[0] - origin[0],
                tile[1] - origin[1],
                tile[2] - origin[0],
                tile[3] - origin[1],
            ];
            let mut remainder = if patch_only {
                if !empty(cut) && source.physical_tile((tx, ty)).is_some() {
                    cut
                } else {
                    [0; 4]
                }
            } else if sparse_reframe && !source.has_physical_tiles(local) {
                [0; 4]
            } else {
                tile
            };
            if empty(bounds) && empty(remainder) {
                continue;
            }
            if empty(remainder) {
                remainder = [0; 4];
            }
            // Only full aligned tiles can be reused when the frame changes.
            // A former edge tile may contain padding outside its logical frame.
            let edge = TILE as i32;
            if sparse_reframe
                && reuse_base_tiles
                && empty(bounds)
                && empty(cut)
                && local[0] >= 0
                && local[1] >= 0
                && local[0] % edge == 0
                && local[1] % edge == 0
                && local[2] - local[0] == edge
                && local[3] - local[1] == edge
                && local[2] <= source.width() as i32
                && local[3] <= source.height() as i32
            {
                let coordinate = (local[0] as u32 / TILE, local[1] as u32 / TILE);
                if let Some(tile) = source.physical_tile(coordinate) {
                    // Frame changes discard explicit empty tiles. Prove the
                    // bytes before retaining a shared physical reference.
                    work.claim(u64::from(TILE) * u64::from(TILE))?;
                    let original = source
                        .prepare_tile(coordinate, &mut || (checks.owner)())
                        .map_err(|e| e.to_string())?;
                    let bytes = original
                        .bytes()
                        .ok_or("Missing unchanged selected source tile")?;
                    if bytes.iter().any(|&byte| byte != 0) {
                        patches.push(TilePatch::stored(
                            (tx, ty),
                            tile.owner().clone(),
                            tile.reference(),
                        ));
                    }
                }
                continue;
            }
            let initial = if same {
                Some(
                    source
                        .prepare_tile((tx, ty), &mut || (checks.owner)())
                        .map_err(|e| e.to_string())?,
                )
            } else {
                None
            };
            let (mut bytes, preserve) = if let Some(tile) = &initial {
                (
                    tile.copy_to_scratch(pool).map_err(|e| e.to_string())?,
                    !tile.is_known_zero(),
                )
            } else {
                (
                    ScratchBytes::new(pool, spec.bytes(), spec.bytes())
                        .map_err(|e| e.to_string())?,
                    false,
                )
            };
            let base = [
                (remainder[0] - origin[0]).max(0),
                (remainder[1] - origin[1]).max(0),
                (remainder[2] - origin[0]).min(source.width() as i32),
                (remainder[3] - origin[1]).min(source.height() as i32),
            ];
            let prepared = if !empty(base) {
                Some(
                    Input::prepare(
                        source,
                        selection,
                        origin,
                        Region::new(base).map_err(|e| e.to_string())?,
                        fill,
                        checks.owner,
                    )
                    .map_err(|e| e.to_string())?,
                )
            } else {
                None
            };
            for y in remainder[1]..remainder[3] {
                (checks.active)().map_err(|e| e.to_string())?;
                for x in remainder[0]..remainder[2] {
                    work.claim(1)?;
                    let (sx, sy) = (x - origin[0], y - origin[1]);
                    let (pixel, cleared) = if sx >= 0
                        && sy >= 0
                        && sx < source.width() as i32
                        && sy < source.height() as i32
                    {
                        prepared
                            .as_ref()
                            .ok_or("Missing selected remainder footprint")?
                            .remainder(sx, sy, work)?
                    } else {
                        ([0; 4], false)
                    };
                    changed |= cleared;
                    put(&mut bytes, source.depth(), (tx, ty), next, x, y, pixel);
                }
            }
            drop(prepared);
            if !empty(bounds) {
                for y in (bounds[1]..bounds[3]).step_by(block) {
                    for x in (bounds[0]..bounds[2]).step_by(block) {
                        render(
                            source,
                            selection,
                            placement,
                            &sampler,
                            origin,
                            fill,
                            [x, y, (x + block as i32).min(bounds[2]), (y + block as i32).min(bounds[3])],
                            (tx, ty),
                            next,
                            &mut bytes,
                            positions.as_deref_mut().unwrap_or_default(),
                            work,
                            checks,
                            &mut changed,
                        )?;
                    }
                }
            }
            if patch_only
                && initial
                    .as_ref()
                    .and_then(|tile| tile.bytes())
                    .is_some_and(|before| before == &*bytes)
            {
                continue;
            }
            if preserve || bytes.iter().any(|v| *v != 0) {
                let reference = store
                    .put_guarded(
                        spec,
                        &bytes,
                        &mut || (checks.active)(),
                        &mut || (checks.owner)(),
                    )
                    .map_err(|e| e.to_string())?;
                if patch_base.is_some() {
                    patches.push(TilePatch::stored((tx, ty), store.clone(), reference));
                } else {
                    blocks.insert((tx, ty), reference);
                }
            }
            drop(initial);
        }
    }
    (checks.owner)().map_err(|e| e.to_string())?;
    let surface = if let Some(base) = patch_base {
        base.with_tile_patches(&patches, pool, selection.document().disk_budget())
            .map_err(|e| e.to_string())?
    } else if blocks.is_empty() {
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

fn coverage_occupied(
    selection: &Selection<'_>,
    [left, top, right, bottom]: [i32; 4],
    work: &mut Work,
) -> Result<bool, String> {
    let Some(mask) = selection.raw_mask() else {
        return Ok(true);
    };
    let [x, y] = selection.origin();
    occupied_source(mask, [left - x, top - y, right - x, bottom - y], work)
}

pub(super) fn occupied_source(source: &Surface, footprint: [i32; 4], work: &mut Work) -> Result<bool, String> {
    work.claim(4)?;
    let footprint = intersect(footprint, [0, 0, source.width() as i32, source.height() as i32]);
    if empty(footprint) {
        return Ok(false);
    }
    let [l, t, r, b] = footprint.map(|v| v as u32);
    let probes = (r.div_ceil(TILE) - l / TILE) * (b.div_ceil(TILE) - t / TILE);
    work.claim(u64::from(probes).min(source.physical_tiles().len() as u64))?;
    Ok(source.has_physical_tiles(footprint))
}

fn intersect(a: [i32; 4], b: [i32; 4]) -> [i32; 4] {
    [
        a[0].max(b[0]),
        a[1].max(b[1]),
        a[2].min(b[2]),
        a[3].min(b[3]),
    ]
}
fn empty(rect: [i32; 4]) -> bool {
    rect[0] >= rect[2] || rect[1] >= rect[3]
}
fn at(tile: (u32, u32), origin: [i32; 2], x: i32, y: i32, depth: u16) -> usize {
    ((y - origin[1] - tile.1 as i32 * TILE as i32) as usize * TILE as usize
        + (x - origin[0] - tile.0 as i32 * TILE as i32) as usize)
        * usize::from(depth / 8)
        * 4
}
fn put(
    bytes: &mut [u8],
    depth: u16,
    tile: (u32, u32),
    origin: [i32; 2],
    x: i32,
    y: i32,
    p: Pixel16,
) {
    let at = at(tile, origin, x, y, depth);
    if depth == 16 {
        for (pair, v) in bytes[at..at + 8].chunks_exact_mut(2).zip(p) {
            pair.copy_from_slice(&v.to_be_bytes());
        }
    } else {
        for (out, v) in bytes[at..at + 4].iter_mut().zip(p) {
            *out = crate::raster::project16(v);
        }
    }
}
fn pixel(bytes: &[u8], depth: u16, tile: (u32, u32), origin: [i32; 2], x: i32, y: i32) -> Pixel16 {
    let at = at(tile, origin, x, y, depth);
    if depth == 16 {
        std::array::from_fn(|i| u16::from_be_bytes([bytes[at + i * 2], bytes[at + i * 2 + 1]]))
    } else {
        std::array::from_fn(|i| u16::from(bytes[at + i]) * 257)
    }
}
#[allow(clippy::too_many_arguments)]
fn render(
    source: &Surface,
    selection: &Selection<'_>,
    placement: &Placement,
    sampler: &Sampler<'_>,
    old: [i32; 2],
    fill: Option<Pixel>,
    region: [i32; 4],
    tile: (u32, u32),
    origin: [i32; 2],
    output: &mut [u8],
    positions: &mut [u8],
    work: &mut Work,
    checks: Checks<'_>,
    changed: &mut bool,
) -> Result<(), String> {
    (checks.active)().map_err(|e| e.to_string())?;
    let position = |x, y| {
        if crate::selection::contains(&placement.polygon, x as f32 + 0.5, y as f32 + 0.5) {
            sampler.sample(x, y)
        } else {
            [-2.; 2]
        }
    };
    let frame = [0, 0, source.width() as i32, source.height() as i32];
    let mapping_cost = placement.polygon.len() as u64 + sampler.sample_cost();
    let affine = sampler.is_affine();
    let footprint = if let Some(bounds) = sampler.affine_footprint(region) {
        work.claim(4)?;
        // The virtual cut is zero outside the selection even when the
        // inverse footprint includes additional source pixels.
        let [left, top, right, bottom] = placement.area;
        intersect(intersect(bounds, frame), [
            left - old[0],
            top - old[1],
            right - old[0],
            bottom - old[1],
        ])
    } else {
        let mut footprint = [i32::MAX, i32::MAX, i32::MIN, i32::MIN];
        let mut i = 0;
        for y in region[1]..region[3] {
            (checks.active)().map_err(|e| e.to_string())?;
            for x in region[0]..region[2] {
                work.claim(mapping_cost)?;
                let p = position(x, y);
                positions[i..i + 4].copy_from_slice(&p[0].to_bits().to_be_bytes());
                positions[i + 4..i + 8].copy_from_slice(&p[1].to_bits().to_be_bytes());
                i += 8;
                if outside(source, p) {
                    continue;
                }
                let (x, y) = (p[0].floor() as i32, p[1].floor() as i32);
                footprint = [
                    footprint[0].min(x),
                    footprint[1].min(y),
                    footprint[2].max(x + 2),
                    footprint[3].max(y + 2),
                ];
            }
        }
        intersect(footprint, frame)
    };
    if empty(footprint) {
        return Ok(());
    }
    let area = (footprint[2] - footprint[0]) as u64 * (footprint[3] - footprint[1]) as u64;
    let prepared = if area > MAX_REGION_PIXELS as u64 {
        None
    } else {
        match Input::prepare(
            source,
            selection,
            old,
            Region::new(footprint).map_err(|e| e.to_string())?,
            fill,
            checks.owner,
        ) {
            Ok(view) => Some(view),
            Err(Error::Budget) => None,
            Err(error) => return Err(error.to_string()),
        }
    };
    let Some(prepared) = prepared else {
        let (w, h) = (region[2] - region[0], region[3] - region[1]);
        if w == 1 && h == 1 {
            return Err("Single selected sample exceeds native footprint admission".into());
        }
        let (mut first, mut second) = (region, region);
        if w >= h {
            first[2] = region[0] + w / 2;
            second[0] = first[2];
        } else {
            first[3] = region[1] + h / 2;
            second[1] = first[3];
        }
        render(
            source, selection, placement, sampler, old, fill, first, tile, origin, output,
            positions, work, checks, changed,
        )?;
        return render(
            source, selection, placement, sampler, old, fill, second, tile, origin, output,
            positions, work, checks, changed,
        );
    };
    let mut i = 0;
    for y in region[1]..region[3] {
        (checks.active)().map_err(|e| e.to_string())?;
        for x in region[0]..region[2] {
            let p = if affine {
                work.claim(mapping_cost)?;
                position(x, y)
            } else {
                let p = crate::transform::native::coord(positions, i);
                i += 1;
                p
            };
            if outside(source, p) {
                continue;
            }
            let top = sample(source.depth(), p, &prepared, work)?;
            if top[3] == 0 {
                continue;
            }
            let before = pixel(output, source.depth(), tile, origin, x, y);
            let after = if source.depth() == 16 {
                crate::raster::blend16(before, top, 1., "normal")
            } else {
                crate::raster::blend(
                    before.map(crate::raster::project16),
                    top.map(crate::raster::project16),
                    1.,
                    "normal",
                )
                .map(|v| u16::from(v) * 257)
            };
            *changed |= before != after;
            put(output, source.depth(), tile, origin, x, y, after);
        }
    }
    Ok(())
}
fn outside(source: &Surface, p: [f32; 2]) -> bool {
    p[0] <= -1. || p[1] <= -1. || p[0] >= source.width() as f32 || p[1] >= source.height() as f32
}
fn sample(
    depth: u16,
    [x, y]: [f32; 2],
    input: &Input<'_>,
    work: &mut Work,
) -> Result<Pixel16, String> {
    if x.fract() == 0. && y.fract() == 0. {
        return input.cut(x as i32, y as i32, work);
    }
    let (ix, iy) = (x.floor() as i32, y.floor() as i32);
    if depth == 16 {
        let (fx, fy) = (f64::from(x - x.floor()), f64::from(y - y.floor()));
        let mut sum = [0.; 4];
        for (dx, dy, w) in [
            (0, 0, (1. - fx) * (1. - fy)),
            (1, 0, fx * (1. - fy)),
            (0, 1, (1. - fx) * fy),
            (1, 1, fx * fy),
        ] {
            let p = input.cut(ix + dx, iy + dy, work)?;
            let a = f64::from(p[3]) / 65535.;
            for c in 0..3 {
                sum[c] += f64::from(p[c]) * a * w;
            }
            sum[3] += a * w;
        }
        Ok(if sum[3] <= 0. {
            [0; 4]
        } else {
            [
                (sum[0] / sum[3]).round().clamp(0., 65535.) as u16,
                (sum[1] / sum[3]).round().clamp(0., 65535.) as u16,
                (sum[2] / sum[3]).round().clamp(0., 65535.) as u16,
                (sum[3] * 65535.).round().clamp(0., 65535.) as u16,
            ]
        })
    } else {
        let (fx, fy) = (x - x.floor(), y - y.floor());
        let mut sum = [0.0f32; 4];
        for (dx, dy, w) in [
            (0, 0, (1. - fx) * (1. - fy)),
            (1, 0, fx * (1. - fy)),
            (0, 1, (1. - fx) * fy),
            (1, 1, fx * fy),
        ] {
            let p = input
                .cut(ix + dx, iy + dy, work)?
                .map(crate::raster::project16);
            let a = f32::from(p[3]) / 255.;
            for c in 0..3 {
                sum[c] += f32::from(p[c]) * a * w;
            }
            sum[3] += a * w;
        }
        Ok(if sum[3] <= 0.00001 {
            [0; 4]
        } else {
            [
                (sum[0] / sum[3]).round() as u8,
                (sum[1] / sum[3]).round() as u8,
                (sum[2] / sum[3]).round() as u8,
                (sum[3] * 255.).round() as u8,
            ]
            .map(|v| u16::from(v) * 257)
        })
    }
}
