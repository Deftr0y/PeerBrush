use super::{
    geometry::{Placement, Sampler},
    input::{Input, Selection},
};
use crate::{
    large_document::{Error, Result as SourceResult, ScratchBytes, Store, TileSpec},
    raster::{
        backed::{Plane, Region, MAX_REGION_PIXELS},
        Pixel, Pixel16, Surface, TILE,
    },
    transform::native::Work,
};
use std::collections::BTreeMap;

pub(crate) struct Written {
    pub surface: Surface,
    pub cleared_retained: bool,
}
pub(crate) fn write(
    source: &Surface,
    selection: &Selection<'_>,
    placement: &Placement,
    origin: [i32; 2],
    fill: Option<Pixel>,
    store: &Store,
    work: &mut Work,
    check: &dyn Fn() -> SourceResult<()>,
) -> Result<Written, String> {
    check().map_err(|e| e.to_string())?;
    if placement.area != selection.bounds
        || !source.uses_budget(selection.document().budget())
        || !source.uses_disk_budget(selection.document().disk_budget())
        || !store
            .resident_budget()
            .same_pool(selection.document().budget())
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
        (origin[0] + source.width() as i32).max(r) - next[0],
        (origin[1] + source.height() as i32).max(b) - next[1],
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
    let mut changed = !same;
    let mut positions =
        ScratchBytes::new(pool, 32 * 32 * 8, 32 * 32 * 8).map_err(|e| e.to_string())?;
    for ty in 0..frame[1].div_ceil(TILE) {
        for tx in 0..frame[0].div_ceil(TILE) {
            check().map_err(|e| e.to_string())?;
            let initial = if same {
                Some(
                    source
                        .prepare_tile((tx, ty), &mut || check())
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
            let tile = [
                (tx * TILE) as i32 + next[0],
                (ty * TILE) as i32 + next[1],
                ((tx * TILE + TILE).min(frame[0])) as i32 + next[0],
                ((ty * TILE + TILE).min(frame[1])) as i32 + next[1],
            ];
            let base = [
                (tile[0] - origin[0]).max(0),
                (tile[1] - origin[1]).max(0),
                (tile[2] - origin[0]).min(source.width() as i32),
                (tile[3] - origin[1]).min(source.height() as i32),
            ];
            let prepared = if base[0] < base[2] && base[1] < base[3] {
                Some(
                    Input::prepare(
                        source,
                        selection,
                        origin,
                        Region::new(base).map_err(|e| e.to_string())?,
                        fill,
                        check,
                    )
                    .map_err(|e| e.to_string())?,
                )
            } else {
                None
            };
            for y in tile[1]..tile[3] {
                check().map_err(|e| e.to_string())?;
                for x in tile[0]..tile[2] {
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
            let bounds = [
                tile[0].max(l),
                tile[1].max(t),
                tile[2].min(r),
                tile[3].min(b),
            ];
            if bounds[0] < bounds[2] && bounds[1] < bounds[3] {
                for y in (bounds[1]..bounds[3]).step_by(32) {
                    for x in (bounds[0]..bounds[2]).step_by(32) {
                        render(
                            source,
                            selection,
                            placement,
                            &sampler,
                            origin,
                            fill,
                            [x, y, (x + 32).min(bounds[2]), (y + 32).min(bounds[3])],
                            (tx, ty),
                            next,
                            &mut bytes,
                            &mut positions,
                            work,
                            check,
                            &mut changed,
                        )?;
                    }
                }
            }
            if preserve || bytes.iter().any(|v| *v != 0) {
                blocks.insert(
                    (tx, ty),
                    store
                        .put(spec, &bytes, &mut || check())
                        .map_err(|e| e.to_string())?,
                );
            }
            drop(initial);
        }
    }
    check().map_err(|e| e.to_string())?;
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
    check().map_err(|e| e.to_string())?;
    Ok(Written {
        surface,
        cleared_retained: changed,
    })
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
    check: &dyn Fn() -> SourceResult<()>,
    changed: &mut bool,
) -> Result<(), String> {
    let mut footprint = [i32::MAX, i32::MAX, i32::MIN, i32::MIN];
    let mut i = 0;
    for y in region[1]..region[3] {
        check().map_err(|e| e.to_string())?;
        for x in region[0]..region[2] {
            work.claim(placement.polygon.len() as u64 + 1)?;
            let p =
                if crate::selection::contains(&placement.polygon, x as f32 + 0.5, y as f32 + 0.5) {
                    sampler.sample(x, y)
                } else {
                    [-2.; 2]
                };
            positions[i..i + 4].copy_from_slice(&p[0].to_bits().to_be_bytes());
            positions[i + 4..i + 8].copy_from_slice(&p[1].to_bits().to_be_bytes());
            i += 8;
            if outside(source, p) {
                continue;
            }
            let (x, y) = (p[0].floor() as i32, p[1].floor() as i32);
            footprint = [
                footprint[0].min(x.max(0)),
                footprint[1].min(y.max(0)),
                footprint[2].max((x + 2).min(source.width() as i32)),
                footprint[3].max((y + 2).min(source.height() as i32)),
            ];
        }
    }
    if footprint[0] >= footprint[2] || footprint[1] >= footprint[3] {
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
            check,
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
            positions, work, check, changed,
        )?;
        return render(
            source, selection, placement, sampler, old, fill, second, tile, origin, output,
            positions, work, check, changed,
        );
    };
    let mut i = 0;
    for y in region[1]..region[3] {
        check().map_err(|e| e.to_string())?;
        for x in region[0]..region[2] {
            let p = [
                f32::from_bits(u32::from_be_bytes(positions[i..i + 4].try_into().unwrap())),
                f32::from_bits(u32::from_be_bytes(
                    positions[i + 4..i + 8].try_into().unwrap(),
                )),
            ];
            i += 8;
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
