//! Candidate-only selection preparation. All source row admission, native
//! pinning and staging happen outside engine/UI locks; publication is separate.
use super::{backed::CoverageSource, coverage_writer::Writer, flood, rows, SelectionSource};
use crate::{
    large_document::{Error, Result, ScratchBytes, Store},
    raster::backed::Region,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Combine {
    Replace,
    Add,
    Subtract,
    Intersect,
}
impl Combine {
    fn value(self, a: u8, b: u8) -> u8 {
        match self {
            Self::Replace => b,
            Self::Add => a.max(b),
            Self::Subtract => ((u16::from(a) * (255 - u16::from(b)) + 127) / 255) as u8,
            Self::Intersect => a.min(b),
        }
    }
}

pub struct Request<'a> {
    pub width: u32,
    pub height: u32,
    pub seeds: &'a [[u32; 2]],
    pub quick_radius: Option<u32>,
    pub feather: u32,
    pub mode: Combine,
    pub maximum_work: u64,
}

/// This produces no document mutation or publication permission. `matches`
/// only reads the immutable source view established by `prepare_row`.
/// Source/revision/lease/cancellation must still pass common engine admission.
pub fn prepare(
    request: &Request<'_>,
    store: Store,
    prior: Option<&SelectionSource>,
    check: &dyn Fn() -> Result<()>,
    mut prepare_row: impl FnMut(u32) -> Result<()>,
    mut matches: impl FnMut(usize, u32, u32) -> Result<bool>,
) -> std::result::Result<CoverageSource, String> {
    if request.seeds.len() > 8192
        || request.feather > 64
        || request.maximum_work == 0
        || request.maximum_work > 1_024_000_000
        || request.quick_radius.is_some_and(|r| r == 0 || r > 256)
    {
        return Err("Invalid admitted selection worker request".into());
    }
    let shape = flood::Layout::new(request.width, request.height).map_err(str::to_owned)?;
    let layout =
        rows::Layout::new(request.width, request.height, rows::MAX_SPANS).map_err(str::to_owned)?;
    check().map_err(|e| e.to_string())?;
    let pool = store.resident_budget();
    let allocate = |bytes| ScratchBytes::new(&pool, bytes, bytes).map_err(|e| e.to_string());
    let mut states = allocate(shape.states)?;
    let mut generations = allocate(shape.generations)?;
    let mut frontier = allocate(flood::MAX_FRONTIER * 4)?;
    let mut records = allocate(layout.records)?;
    let mut indices = allocate(layout.indices)?;
    let mut ends = allocate(layout.ends)?;
    let mut spans = rows::Spans::new(
        layout,
        &mut records,
        &mut indices,
        &mut ends,
        request.maximum_work,
    )
    .map_err(str::to_owned)?;
    let mut writer =
        Writer::new(request.width, request.height, store, &pool).map_err(|e| e.to_string())?;
    let completed = {
        let mut flood = flood::Flood::new(
            shape,
            &mut states,
            &mut generations,
            &mut frontier,
            request.maximum_work,
        )
        .map_err(str::to_owned)?;
        flood
            .run_rows(
                request.seeds,
                request.quick_radius,
                check,
                |y| prepare_row(y),
                |s, x, y| matches(s, x, y),
                |y, l, r| spans.push(y, l, r).map_err(Error::Invalid),
            )
            .map_err(|e| e.to_string())?
    };
    // The traversal's large workspace is no longer needed by the ordered rows.
    drop(states);
    drop(generations);
    drop(frontier);
    let scalar_complete = {
        let ordered = spans.finish(&completed, || check().map_err(|e| e.to_string()))?;
        let width = request.width as usize;
        let mut ring = allocate((2 * request.feather as usize + 1) * width)?;
        let mut sums = allocate(width * 4)?;
        let mut input = allocate(width)?;
        let mut output = allocate(width)?;
        let mut previous = allocate(width)?;
        let mut combined = allocate(width)?;
        let mut feather = rows::Feather::new(
            request.width,
            request.height,
            request.feather,
            &mut ring,
            &mut sums,
            &mut input,
            &mut output,
        )
        .map_err(str::to_owned)?;
        feather.run(
            || check().map_err(|e| e.to_string()),
            |y, row| ordered.row(y, row, || check().map_err(|e| e.to_string())),
            |y, row| {
                if request.mode == Combine::Replace {
                    return writer
                        .row(y, row, &mut || check())
                        .map_err(|e| e.to_string());
                }
                // No restriction is not a previous selection: the historical
                // combination baseline is zero until a selection actually exists.
                previous.fill(0);
                if let Some(prior) = prior.filter(|p| !matches!(p, SelectionSource::All)) {
                    let view = prior
                        .prepare_region(
                            Region::new([0, y as i32, request.width as i32, y as i32 + 1])
                                .map_err(|e| e.to_string())?,
                            &pool,
                            &mut || check(),
                        )
                        .map_err(|e| e.to_string())?;
                    view.copy_row(y as i32, &mut previous)
                        .map_err(|e| e.to_string())?;
                }
                for ((value, &a), &b) in combined.iter_mut().zip(previous.iter()).zip(row) {
                    *value = request.mode.value(a, b);
                }
                writer
                    .row(y, &combined, &mut || check())
                    .map_err(|e| e.to_string())
            },
        )?
    };
    drop(records);
    drop(indices);
    drop(ends);
    check().map_err(|e| e.to_string())?;
    let candidate = writer
        .finish(&scalar_complete, &mut || check())
        .map_err(|e| e.to_string())?;
    check().map_err(|e| e.to_string())?;
    Ok(candidate)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        large_document::{Limits, ResidentBudget},
        raster::{project16, surface::Surface, Raster},
    };
    use std::cell::RefCell;
    fn store(pool: ResidentBudget) -> (Store, std::path::PathBuf) {
        let path = std::env::temp_dir().join(format!(
            "peerbrush-selection-worker-{}",
            crate::engine::id()
        ));
        let store = Store::create(
            &path,
            pool,
            Limits {
                disk_bytes: 32 * 1024 * 1024,
                max_blocks: 64,
                max_resident_blocks: 32,
                manifest_bytes: 1024 * 1024,
            },
        )
        .unwrap();
        (store, path)
    }
    #[test]
    fn native16_source_rows_flood_into_exact_coverage_without_changing_original_samples() {
        let (w, h) = (257u32, 259u32);
        let pool = ResidentBudget::new(128 * 1024 * 1024).unwrap();
        let (store, path) = store(pool.clone());
        let mut raster = Raster::new_depth(w, h, 16);
        let colors = [[10001, 20003, 30007, 65535], [40009, 50011, 60013, 65535]];
        for y in 0..h {
            for x in 0..w {
                raster.set16(
                    x as i32,
                    y as i32,
                    colors[usize::from((x < 128) != (y < 129))],
                );
            }
        }
        let source = Surface::stage_raster(&raster, store.clone(), &mut || Ok(())).unwrap();
        let cache = RefCell::new(None::<crate::raster::surface::SurfaceRegion>);
        let request = Request {
            width: w,
            height: h,
            seeds: &[[1, 1], [200, 200]],
            quick_radius: None,
            feather: 0,
            mode: Combine::Replace,
            maximum_work: 1_000_000,
        };
        let candidate = prepare(
            &request,
            store.clone(),
            None,
            &|| Ok(()),
            |y| {
                let top = y / 8 * 8;
                *cache.borrow_mut() = Some(source.prepare_region(
                    Region::new([0, top as i32, w as i32, (top + 8).min(h) as i32])?,
                    &mut || Ok(()),
                )?);
                Ok(())
            },
            |_, x, y| {
                let cache = cache.borrow();
                let view = cache
                    .as_ref()
                    .ok_or(Error::Invalid("Row was not prepared"))?;
                let pixel = view.pixel16(x as i32, y as i32)?;
                Ok(pixel.map(project16) == colors[0].map(project16))
            },
        )
        .unwrap();
        assert_eq!(candidate.bounds(), [0, 0, w as i32, h as i32]);
        assert_eq!(candidate.contours().len(), 2);
        drop(cache);
        for y in 0..h {
            let region = Region::new([0, y as i32, w as i32, y as i32 + 1]).unwrap();
            let mask = candidate
                .prepare_region(region, &pool, &mut || Ok(()))
                .unwrap();
            let original = source.prepare_region(region, &mut || Ok(())).unwrap();
            for x in 0..w {
                assert_eq!(
                    mask.value(x as i32, y as i32).unwrap(),
                    if (x < 128) == (y < 129) { 1. } else { 0. }
                );
                assert_eq!(
                    original.pixel16(x as i32, y as i32).unwrap(),
                    colors[usize::from((x < 128) != (y < 129))]
                );
            }
        }
        drop(candidate);
        drop(source);
        drop(store);
        assert_eq!(pool.used(), 0);
        std::fs::remove_dir_all(path).unwrap();
    }
    #[test]
    fn scalar_combination_matches_every_legacy_byte_pair_including_subtract_rounding() {
        for a in 0..=255u8 {
            for b in 0..=255u8 {
                let legacy =
                    ((f32::from(a) / 255.) * (1. - f32::from(b) / 255.) * 255.).round() as u8;
                assert_eq!(Combine::Subtract.value(a, b), legacy);
                assert_eq!(Combine::Add.value(a, b), a.max(b));
                assert_eq!(Combine::Intersect.value(a, b), a.min(b));
            }
        }
    }
    #[test]
    fn failed_explicit_source_row_returns_no_candidate_and_releases_workspace() {
        let pool = ResidentBudget::new(128 * 1024 * 1024).unwrap();
        let (store, path) = store(pool.clone());
        let request = Request {
            width: 257,
            height: 259,
            seeds: &[[0, 0]],
            quick_radius: None,
            feather: 3,
            mode: Combine::Replace,
            maximum_work: 1_000_000,
        };
        let result = prepare(
            &request,
            store.clone(),
            None,
            &|| Ok(()),
            |y| if y == 17 { Err(Error::Changed) } else { Ok(()) },
            |_, _, _| Ok(true),
        );
        assert!(result.is_err());
        assert_eq!(pool.used(), 0);
        drop(store);
        std::fs::remove_dir_all(path).unwrap();
    }

    #[test]
    fn actual_row_combination_preserves_empty_baseline_and_geometric_prior_semantics() {
        let pool = ResidentBudget::new(128 * 1024 * 1024).unwrap();
        let (store, path) = store(pool.clone());
        let geometry = SelectionSource::from_metadata(Some([2, 3, 11, 9]), None, None).unwrap();
        let all = SelectionSource::All;
        for (prior_index, prior) in [None, Some(&all), Some(&geometry)].into_iter().enumerate() {
            for mode in [
                Combine::Replace,
                Combine::Add,
                Combine::Subtract,
                Combine::Intersect,
            ] {
                let request = Request {
                    width: 16,
                    height: 12,
                    seeds: &[[0, 0]],
                    quick_radius: None,
                    feather: 0,
                    mode,
                    maximum_work: 1000,
                };
                let source = prepare(
                    &request,
                    store.clone(),
                    prior,
                    &|| Ok(()),
                    |_| Ok(()),
                    |_, _, _| Ok(true),
                )
                .unwrap();
                let view = source
                    .prepare_region(Region::new([0, 0, 16, 12]).unwrap(), &pool, &mut || Ok(()))
                    .unwrap();
                for y in 0..12 {
                    for x in 0..16 {
                        let expected = match mode {
                            Combine::Replace | Combine::Add => 1.,
                            Combine::Subtract => 0.,
                            Combine::Intersect => {
                                if prior_index == 2 && (2..11).contains(&x) && (3..9).contains(&y) {
                                    1.
                                } else {
                                    0.
                                }
                            }
                        };
                        assert_eq!(
                            view.value(x, y).unwrap(),
                            expected,
                            "prior={prior_index},mode={mode:?},x={x},y={y}"
                        );
                    }
                }
                drop(view);
                drop(source);
                assert_eq!(pool.used(), 0);
            }
        }
        drop(store);
        std::fs::remove_dir_all(path).unwrap();
    }

    #[test]
    fn workspace_refusal_occurs_before_source_preparation_or_matching() {
        let pool = ResidentBudget::new(1024).unwrap();
        let (store, path) = store(pool.clone());
        let calls = std::cell::Cell::new(0);
        let request = Request {
            width: 16,
            height: 12,
            seeds: &[[0, 0]],
            quick_radius: None,
            feather: 0,
            mode: Combine::Replace,
            maximum_work: 1000,
        };
        let result = prepare(
            &request,
            store.clone(),
            None,
            &|| Ok(()),
            |_| {
                calls.set(calls.get() + 1);
                Ok(())
            },
            |_, _, _| {
                calls.set(calls.get() + 1);
                Ok(true)
            },
        );
        assert!(result.is_err());
        assert_eq!(calls.get(), 0);
        assert_eq!(pool.used(), 0);
        drop(store);
        std::fs::remove_dir_all(path).unwrap();
    }
}
