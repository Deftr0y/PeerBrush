use super::{
    geometry::Sampler,
    input::{Prepared, Selection},
    Placement,
};
use crate::{
    large_document::{Error, Result as SourceResult, ScratchBytes, Store},
    raster::backed::{Region, MAX_REGION_PIXELS},
    selection::backed::CoverageSource,
    transform::native::Work,
};

pub(crate) fn write_coverage(
    selection: &Selection<'_>,
    placement: &Placement,
    store: &Store,
    work: &mut Work,
    check: &dyn Fn() -> SourceResult<()>,
) -> Result<CoverageSource, String> {
    check().map_err(|e| e.to_string())?;
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
    let mut positions = ScratchBytes::new(pool, 128 * 8, 128 * 8).map_err(|e| e.to_string())?;
    let sampler = placement.sampler(selection.origin(), true)?;
    let coverage =
        crate::selection::stage_rows(width, height, store.clone(), pool, check, |y, row| {
            for start in (0..width as usize).step_by(128) {
                let end = (start + 128).min(width as usize);
                sample_row(
                    selection,
                    &sampler,
                    top + y as i32,
                    left + start as i32,
                    &mut row[start..end],
                    &mut positions,
                    work,
                    check,
                )?;
            }
            Ok(())
        })?;
    coverage
        .translate_unique([left, top])
        .map_err(|e| e.to_string())
}
fn sample_row(
    selection: &Selection<'_>,
    sampler: &Sampler<'_>,
    y: i32,
    left: i32,
    output: &mut [u8],
    positions: &mut [u8],
    work: &mut Work,
    check: &dyn Fn() -> SourceResult<()>,
) -> Result<(), String> {
    check().map_err(|e| e.to_string())?;
    let frame = selection.mask_frame();
    let mut bounds = [i32::MAX, i32::MAX, i32::MIN, i32::MIN];
    for (i, _) in output.iter().enumerate() {
        work.claim(1)?;
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
            check,
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
        sample_row(selection, sampler, y, left, first, positions, work, check)?;
        return sample_row(
            selection,
            sampler,
            y,
            left + middle as i32,
            second,
            positions,
            work,
            check,
        );
    };
    for (i, value) in output.iter_mut().enumerate() {
        let [x, y] = [
            f32::from_bits(u32::from_be_bytes(
                positions[i * 8..i * 8 + 4].try_into().unwrap(),
            )),
            f32::from_bits(u32::from_be_bytes(
                positions[i * 8 + 4..i * 8 + 8].try_into().unwrap(),
            )),
        ];
        if outside(frame, [x, y]) {
            *value = 0;
            continue;
        }
        let (ix, iy) = (x.floor() as i32, y.floor() as i32);
        let (fx, fy) = (x - ix as f32, y - iy as f32);
        let mut at = |x, y| byte(&prepared, frame, origin, x, y, work).map(f32::from);
        // Keep the existing scalar expression's left-to-right f32 arithmetic;
        // RGBA sampling would incorrectly unpremultiply these soft values.
        *value = (at(ix, iy)? * (1. - fx) * (1. - fy)
            + at(ix + 1, iy)? * fx * (1. - fy)
            + at(ix, iy + 1)? * (1. - fx) * fy
            + at(ix + 1, iy + 1)? * fx * fy)
            .round() as u8;
    }
    check().map_err(|e| e.to_string())
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
