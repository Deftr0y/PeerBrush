use super::{
    geometry::Sampler,
    input::{Prepared, Selection},
    render::occupied_source,
    Placement,
};
use crate::{
    large_document::{Error, ScratchBytes, Store},
    raster::{backed::{Region, MAX_REGION_PIXELS}, TILE},
    selection::backed::CoverageSource,
    transform::native::{Checks, Work},
};

const BAND_ROWS: u32 = 32;
const ROW_CHUNK: usize = 128;
const _: () = assert!(TILE % BAND_ROWS == 0);

pub(crate) fn write_coverage(
    selection: &Selection<'_>,
    placement: &Placement,
    store: &Store,
    work: &mut Work,
    checks: Checks<'_>,
) -> Result<CoverageSource, String> {
    (checks.owner)().map_err(|e| e.to_string())?;
    if !selection.rasterized()
        || selection.bounds != placement.area
        || !store
            .resident_budget()
            .same_pool(selection.document().budget())
        || store
            .disk_budget()
            .is_none_or(|p| !p.same_pool(selection.document().disk_budget()))
    {
        return Err("Transformed scalar coverage admission differs".into());
    }
    let [left, top, right, bottom] = placement.bounds;
    let (width, height) = ((right - left) as u32, (bottom - top) as u32);
    if width == 0 || height == 0 || width > 16000 || height > 16000 {
        return Err("Transformed coverage exceeds native shape admission".into());
    }
    let pool = selection.document().budget();
    let sampler = placement.sampler(selection.origin(), true)?;
    let mut band = None;
    let mut positions = None;
    let columns = width.div_ceil(TILE) as usize;
    let occupied = if let Some(mask) = selection.raw_mask() {
        let rows = height.div_ceil(TILE) as usize;
        let count = columns * rows;
        let mut occupied = ScratchBytes::new(pool, count, count).map_err(|e| e.to_string())?;
        let edge = TILE as i32;
        for (i, tile) in occupied.iter_mut().enumerate() {
            (checks.active)().map_err(|e| e.to_string())?;
            let (x, y) = (left + (i % columns) as i32 * edge, top + (i / columns) as i32 * edge);
            if !sampler.is_affine() {
                work.claim(4 * sampler.sample_cost())?;
            }
            let footprint =
                sampler.source_footprint([x, y, (x + edge).min(right), (y + edge).min(bottom)]);
            // Coverage uses the complete raw mask, including any taps beyond
            // the selection bounds hint.
            *tile = u8::from(match footprint {
                Some(footprint) => occupied_source(mask, footprint, work)?,
                None => true,
            });
        }
        Some(occupied)
    } else {
        None
    };
    let coverage = crate::selection::stage_rows_guarded(
        width, height, store.clone(), pool, checks.active, checks.owner, |y, row| {
            let tile_row = y as usize / TILE as usize * columns;
            if y == 0 {
                if sampler.is_affine() {
                    // Admit the optional band after the required output writer.
                    let bytes = width as usize * height.min(BAND_ROWS) as usize;
                    match ScratchBytes::new(pool, bytes, bytes) {
                        Ok(scratch) => band = Some(scratch),
                        Err(Error::Budget) => {}
                        Err(error) => return Err(error.to_string()),
                    }
                }
                if band.is_none() {
                    let bytes = ROW_CHUNK * 8;
                    positions = Some(ScratchBytes::new(pool, bytes, bytes).map_err(|e| e.to_string())?);
                }
            }
            if let Some(scratch) = &mut band {
                let stride = width as usize;
                let band_row = (y % BAND_ROWS) as usize;
                if band_row == 0 {
                    let end = (y + BAND_ROWS).min(height);
                    scratch.fill(0);
                    for start in (0..width).step_by(TILE as usize) {
                        let column = (start / TILE) as usize;
                        // BAND_ROWS divides TILE, so a band stays in one tile row.
                        if occupied.as_ref().is_some_and(|tiles| tiles[tile_row + column] == 0) {
                            continue;
                        }
                        let bounds = [
                            left + start as i32,
                            top + y as i32,
                            left + (start + TILE).min(width) as i32,
                            top + end as i32,
                        ];
                        sample_band(
                            selection,
                            &sampler,
                            bounds,
                            stride,
                            &mut scratch[start as usize..],
                            work,
                            checks,
                        )?;
                    }
                }
                row.copy_from_slice(&scratch[band_row * stride..(band_row + 1) * stride]);
                if y + 1 == height {
                    band = None;
                }
                return Ok(());
            }
            for start in (0..width as usize).step_by(ROW_CHUNK) {
                let end = (start + ROW_CHUNK).min(width as usize);
                if occupied.as_ref().is_some_and(|tiles| {
                    tiles[tile_row + start / TILE as usize..=tile_row + (end - 1) / TILE as usize]
                        .iter().all(|&tile| tile == 0)
                }) {
                    row[start..end].fill(0);
                    continue;
                }
                sample_row(
                    selection,
                    &sampler,
                    top + y as i32,
                    left + start as i32,
                    &mut row[start..end],
                    positions.as_deref_mut().ok_or("Missing coverage coordinate scratch")?,
                    work,
                    checks,
                )?;
            }
            Ok(())
        },
    )?;
    coverage
        .translate_unique([left, top])
        .map_err(|e| e.to_string())
}

fn sample_band(
    selection: &Selection<'_>,
    sampler: &Sampler<'_>,
    region: [i32; 4],
    stride: usize,
    output: &mut [u8],
    work: &mut Work,
    checks: Checks<'_>,
) -> Result<(), String> {
    (checks.active)().map_err(|e| e.to_string())?;
    work.claim(4)?;
    let frame = selection.mask_frame();
    let origin = selection.origin();
    let [l, t, r, b] = sampler.affine_footprint(region).ok_or("Missing affine coverage footprint")?;
    let [l, t, r, b] = [l.max(0), t.max(0), r.min(frame[0] as i32), b.min(frame[1] as i32)];
    if l >= r || t >= b {
        return Ok(());
    }
    let pixels = u64::from((r - l) as u32) * u64::from((b - t) as u32);
    let prepared = if pixels > MAX_REGION_PIXELS as u64 {
        None
    } else {
        let bounds = [l + origin[0], t + origin[1], r + origin[0], b + origin[1]];
        match selection.prepare(Region::new(bounds).map_err(|e| e.to_string())?, checks.owner) {
            Ok(prepared) => Some(prepared),
            Err(Error::Budget) => None,
            Err(error) => return Err(error.to_string()),
        }
    };
    let [left, top, right, bottom] = region;
    let Some(prepared) = prepared else {
        let (width, height) = (right - left, bottom - top);
        if width == 1 && height == 1 {
            return Err("Single coverage sample exceeds native footprint admission".into());
        }
        let (mut first, mut second) = (region, region);
        let offset = if width >= height {
            first[2] = left + width / 2;
            second[0] = first[2];
            (width / 2) as usize
        } else {
            first[3] = top + height / 2;
            second[1] = first[3];
            (height / 2) as usize * stride
        };
        sample_band(selection, sampler, first, stride, output, work, checks)?;
        return sample_band(selection, sampler, second, stride, &mut output[offset..], work, checks);
    };
    for y in top..bottom {
        (checks.active)().map_err(|e| e.to_string())?;
        let at = (y - top) as usize * stride;
        for (i, value) in output[at..at + (right - left) as usize].iter_mut().enumerate() {
            work.claim(1)?;
            *value = sample(&prepared, frame, origin, sampler.sample(left + i as i32, y), work)?;
        }
    }
    Ok(())
}

fn sample_row(
    selection: &Selection<'_>,
    sampler: &Sampler<'_>,
    y: i32,
    left: i32,
    output: &mut [u8],
    positions: &mut [u8],
    work: &mut Work,
    checks: Checks<'_>,
) -> Result<(), String> {
    (checks.active)().map_err(|e| e.to_string())?;
    let frame = selection.mask_frame();
    let mut bounds = [i32::MAX, i32::MAX, i32::MIN, i32::MIN];
    work.claim(output.len() as u64 * sampler.sample_cost())?;
    for (i, _) in output.iter().enumerate() {
        let p = sampler.sample(left + i as i32, y);
        positions[i * 8..i * 8 + 4].copy_from_slice(&p[0].to_bits().to_be_bytes());
        positions[i * 8 + 4..i * 8 + 8].copy_from_slice(&p[1].to_bits().to_be_bytes());
        if outside(frame, p) {
            continue;
        }
        let (x, y) = (p[0].floor() as i32, p[1].floor() as i32);
        bounds = [
            bounds[0].min(x.max(0)),
            bounds[1].min(y.max(0)),
            bounds[2].max((x + 2).min(frame[0] as i32)),
            bounds[3].max((y + 2).min(frame[1] as i32)),
        ];
    }
    if bounds[0] >= bounds[2] || bounds[1] >= bounds[3] {
        output.fill(0);
        return Ok(());
    }
    let origin = selection.origin();
    let area = (bounds[2] - bounds[0]) as u64 * (bounds[3] - bounds[1]) as u64;
    let prepared = if area > MAX_REGION_PIXELS as u64 {
        None
    } else {
        match selection.prepare(
            Region::new([
                bounds[0] + origin[0],
                bounds[1] + origin[1],
                bounds[2] + origin[0],
                bounds[3] + origin[1],
            ])
            .map_err(|e| e.to_string())?,
            checks.owner,
        ) {
            Ok(prepared) => Some(prepared),
            Err(Error::Budget) => None,
            Err(error) => return Err(error.to_string()),
        }
    };
    let Some(prepared) = prepared else {
        if output.len() == 1 {
            return Err("Single coverage sample exceeds native footprint admission".into());
        }
        let middle = output.len() / 2;
        let (first, second) = output.split_at_mut(middle);
        sample_row(selection, sampler, y, left, first, positions, work, checks)?;
        return sample_row(
            selection,
            sampler,
            y,
            left + middle as i32,
            second,
            positions,
            work,
            checks,
        );
    };
    for (i, value) in output.iter_mut().enumerate() {
        *value = sample(&prepared, frame, origin, crate::transform::native::coord(positions, i), work)?;
    }
    (checks.active)().map_err(|e| e.to_string())
}
fn sample(
    prepared: &Prepared<'_>,
    frame: [u32; 2],
    origin: [i32; 2],
    [x, y]: [f32; 2],
    work: &mut Work,
) -> Result<u8, String> {
    if outside(frame, [x, y]) {
        return Ok(0);
    }
    let (ix, iy) = (x.floor() as i32, y.floor() as i32);
    let (fx, fy) = (x - ix as f32, y - iy as f32);
    if fx == 0. && fy == 0. {
        return byte(prepared, frame, origin, ix, iy, work);
    }
    let mut at = |x, y| byte(prepared, frame, origin, x, y, work).map(f32::from);
    // Keep the original scalar expression and operation order for soft coverage.
    Ok((at(ix, iy)? * (1. - fx) * (1. - fy)
        + at(ix + 1, iy)? * fx * (1. - fy)
        + at(ix, iy + 1)? * (1. - fx) * fy
        + at(ix + 1, iy + 1)? * fx * fy)
        .round() as u8)
}
fn byte(
    prepared: &Prepared<'_>,
    frame: [u32; 2],
    origin: [i32; 2],
    x: i32,
    y: i32,
    work: &mut Work,
) -> Result<u8, String> {
    if x < 0 || y < 0 || x >= frame[0] as i32 || y >= frame[1] as i32 {
        return Ok(0);
    }
    prepared.byte(x + origin[0], y + origin[1], work)
}
fn outside(frame: [u32; 2], p: [f32; 2]) -> bool {
    p[0] <= -1. || p[1] <= -1. || p[0] >= frame[0] as f32 || p[1] >= frame[1] as f32
}
