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

/// Bind candidate preparation to one captured scene and its actual shared
/// pools. This still grants no engine/history publication authority.
pub fn prepare_for_scene(
    scene: &crate::engine::scene::SceneSnapshot,
    expected: &crate::engine::scene::SceneIdentity,
    request: &Request<'_>,
    store: Store,
    check: &dyn Fn() -> Result<()>,
    prepare_row: impl FnMut(u32) -> Result<()>,
    matches: impl FnMut(usize, u32, u32) -> Result<bool>,
) -> std::result::Result<CoverageSource, String> {
    let metadata = scene.metadata();
    if scene.identity() != expected
        || request.width != metadata.width
        || request.height != metadata.height
        || !store.resident_budget().same_pool(scene.budget())
        || !store.uses_disk_budget(scene.disk_budget())
    {
        return Err(
            "Selection candidate differs from its captured scene or shared admission pools".into(),
        );
    }
    check().map_err(|e| e.to_string())?;
    // This scope owns the only polygon copy; it drops before its reservation.
    let mut polygon_charge = None;
    let prior = if request.mode == Combine::Replace {
        None
    } else {
        let coverage = metadata
            .selection_coverage
            .as_ref()
            .filter(|coverage| metadata.selection == Some(coverage.bounds))
            .map(|coverage| {
                if coverage.mask.retained.is_some() {
                    return Err(
                        "Retained coverage needs its complete native scene source".to_owned()
                    );
                }
                CoverageSource::new(
                    scene.raster(&coverage.mask)?.clone(),
                    coverage.bounds,
                    coverage.origin,
                    &coverage.contours,
                    scene.budget(),
                )
                .map_err(|e| e.to_string())
            })
            .transpose()?;
        let polygon = if coverage.is_none() {
            metadata
                .selection_polygon
                .as_ref()
                .filter(|points| super::bounds(points).ok() == metadata.selection)
                .map(|points| {
                    let bytes = points
                        .len()
                        .checked_mul(std::mem::size_of::<[f32; 2]>())
                        .ok_or(Error::Budget)?;
                    let charge = scene.budget().reserve_bytes(bytes, 4 * 1024 * 1024)?;
                    let mut copy = Vec::new();
                    copy.try_reserve_exact(points.len())
                        .map_err(|_| Error::Budget)?;
                    if copy
                        .capacity()
                        .checked_mul(std::mem::size_of::<[f32; 2]>())
                        .is_none_or(|capacity| capacity > charge.bytes())
                    {
                        return Err(Error::Budget);
                    }
                    copy.extend_from_slice(points);
                    polygon_charge = Some(charge);
                    Ok(std::sync::Arc::new(copy))
                })
                .transpose()
                .map_err(|e: Error| e.to_string())?
        } else {
            None
        };
        Some(
            SelectionSource::from_metadata(metadata.selection, polygon, coverage)
                .map_err(|e| e.to_string())?,
        )
    };
    let candidate = prepare(request, store, prior.as_ref(), check, prepare_row, matches)?;
    check().map_err(|e| e.to_string())?;
    if !candidate.uses_budget(scene.budget())
        || !candidate.mask().uses_disk_budget(scene.disk_budget())
    {
        return Err("Prepared selection escaped its captured scene admission pools".into());
    }
    drop(prior);
    drop(polygon_charge);
    Ok(candidate)
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
        let path = std::fs::canonicalize(std::env::temp_dir())
            .unwrap()
            .join(format!(
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

    #[test]
    fn scene_bound_worker_refuses_foreign_identity_dimensions_or_pools_before_source_callbacks() {
        use crate::engine::scene::{
            Document36, PendingScene, RasterCollector, RasterRef36, RasterRoster, SceneIdentity,
            SceneSnapshot,
        };
        use crate::large_document::DiskBudget;
        struct Collector(Vec<Surface>);
        impl RasterCollector for Collector {
            fn resident(&mut self, raster: &Raster) -> std::result::Result<RasterRef36, String> {
                self.surface(
                    &Surface::zero(raster.width, raster.height, raster.depth)
                        .map_err(|e| e.to_string())?,
                )
            }
            fn surface(&mut self, source: &Surface) -> std::result::Result<RasterRef36, String> {
                self.0.push(source.clone());
                Ok(RasterRef36 {
                    id: self.0.len() as u32,
                    width: source.width(),
                    height: source.height(),
                    depth: source.depth(),
                    retained: None,
                })
            }
        }
        let pool = ResidentBudget::new(128 * 1024 * 1024).unwrap();
        let disk = DiskBudget::new(32 * 1024 * 1024, 8).unwrap();
        let mut doc = crate::engine::Document::new(16, 12).unwrap();
        doc.selection = Some([0, 0, 16, 12]);
        doc.selection_polygon = Some(vec![[0., 0.], [16., 0.], [0., 12.]]);
        let mut collector = Collector(Vec::new());
        let owner =
            crate::psd::memory36::MetadataCharge::reserve(&pool, 1024 * 1024, &|| Ok(())).unwrap();
        let metadata = Document36::capture(&doc, &mut collector).unwrap();
        let metadata =
            crate::psd::memory36::Admitted::from_parts(std::sync::Arc::new(metadata), owner);
        let identity = SceneIdentity {
            project_id: crate::engine::id(),
            document_id: doc.id.clone(),
            revision: doc.revision,
        };
        let scene = SceneSnapshot::new(
            identity.clone(),
            PendingScene::new(
                metadata,
                RasterRoster::new(collector.0).unwrap(),
                pool.clone(),
                disk.clone(),
            )
            .unwrap(),
            vec![],
            None,
            None,
        )
        .unwrap();
        let path = std::fs::canonicalize(std::env::temp_dir())
            .unwrap()
            .join(format!("peerbrush-selection-scene-{}", crate::engine::id()));
        let limits = Limits {
            disk_bytes: 16 * 1024 * 1024,
            max_blocks: 32,
            max_resident_blocks: 16,
            manifest_bytes: 1024 * 1024,
        };
        let output =
            Store::create_with_disk_budget(&path, pool.clone(), disk.clone(), limits).unwrap();
        let calls = std::cell::Cell::new(0);
        for wrong in [0, 1, 2, 3] {
            let mut expected = identity.clone();
            if wrong == 0 {
                expected.revision += 1;
            }
            let request = Request {
                width: if wrong == 1 { 17 } else { 16 },
                height: 12,
                seeds: &[[0, 0]],
                quick_radius: None,
                feather: 0,
                mode: Combine::Replace,
                maximum_work: 1000,
            };
            let foreign_path = std::fs::canonicalize(std::env::temp_dir())
                .unwrap()
                .join(format!(
                    "peerbrush-selection-foreign-{}",
                    crate::engine::id()
                ));
            let store = if wrong >= 2 {
                Store::create_with_disk_budget(
                    &foreign_path,
                    if wrong == 3 {
                        ResidentBudget::new(pool.limit()).unwrap()
                    } else {
                        pool.clone()
                    },
                    if wrong == 2 {
                        DiskBudget::new(32 * 1024 * 1024, 8).unwrap()
                    } else {
                        disk.clone()
                    },
                    limits,
                )
                .unwrap()
            } else {
                output.clone()
            };
            let result = prepare_for_scene(
                &scene,
                &expected,
                &request,
                store,
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
            if wrong >= 2 {
                std::fs::remove_dir_all(foreign_path).unwrap();
            }
        }
        let request = Request {
            width: 16,
            height: 12,
            seeds: &[[0, 0]],
            quick_radius: None,
            feather: 0,
            mode: Combine::Replace,
            maximum_work: 1000,
        };
        let candidate = prepare_for_scene(
            &scene,
            &identity,
            &request,
            output.clone(),
            &|| Ok(()),
            |_| Ok(()),
            |_, _, _| Ok(true),
        )
        .unwrap();
        assert!(candidate.uses_budget(&pool));
        assert!(candidate.mask().uses_disk_budget(&disk));
        drop(candidate);
        let candidate = prepare_for_scene(
            &scene,
            &identity,
            &Request {
                mode: Combine::Add,
                ..request
            },
            output.clone(),
            &|| Ok(()),
            |_| Ok(()),
            |_, _, _| Ok(false),
        )
        .unwrap();
        let view = candidate
            .prepare_region(Region::new([0, 0, 16, 12]).unwrap(), &pool, &mut || Ok(()))
            .unwrap();
        for y in 0..12 {
            for x in 0..16 {
                let inside = crate::selection::contains(
                    doc.selection_polygon.as_ref().unwrap(),
                    x as f32 + 0.5,
                    y as f32 + 0.5,
                );
                assert_eq!(view.value(x, y).unwrap(), f32::from(u8::from(inside)));
            }
        }
        drop(view);
        drop(candidate);
        drop(scene);
        drop(output);
        assert_eq!(pool.used(), 0);
        std::fs::remove_dir_all(path).unwrap();
    }

    #[test]
    #[ignore = "Opt-in real2GB native16 selection pipeline; component only, not editor/Save/history qualification"]
    fn dense_16000_native16_owned_source_selection_preserves_full_file_and_native_hashes() {
        use crate::{
            large_document::{DiskBudget, TileSpec},
            raster::{backed::Plane, surface::SurfaceRegion, TILE},
        };
        use ring::digest::{Context, SHA256};
        use std::{cell::Cell, collections::BTreeMap, io::Read, time::Instant};
        fn native(x: u32, y: u32) -> [u16; 4] {
            let mut value = (u64::from(x) | (u64::from(y) << 16)).wrapping_mul(0x9e3779b185ebca87);
            value ^= value >> 29;
            [
                value as u16,
                (value >> 16) as u16,
                (value >> 32) as u16,
                65535,
            ]
        }
        fn finish(hash: Context) -> [u8; 32] {
            hash.finish().as_ref().try_into().unwrap()
        }
        fn hex(hash: [u8; 32]) -> String {
            hash.iter().map(|b| format!("{b:02x}")).collect()
        }
        fn file_hash(path: &std::path::Path) -> [u8; 32] {
            let mut file = std::fs::File::open(path).unwrap();
            let mut buffer = [0; 65536];
            let mut hash = Context::new(&SHA256);
            loop {
                let n = file.read(&mut buffer).unwrap();
                if n == 0 {
                    break;
                }
                hash.update(&buffer[..n]);
            }
            finish(hash)
        }
        fn native_hash(source: &Surface) -> [u8; 32] {
            let mut hash = Context::new(&SHA256);
            hash.update(b"PeerBrush-dense-selection-native-tiles-v1\0");
            for ty in 0..source.height().div_ceil(TILE) {
                for tx in 0..source.width().div_ceil(TILE) {
                    let tile = source.prepare_tile((tx, ty), &mut || Ok(())).unwrap();
                    hash.update(&tx.to_be_bytes());
                    hash.update(&ty.to_be_bytes());
                    hash.update(tile.bytes().expect("Dense source tile cannot be absent"));
                }
            }
            finish(hash)
        }
        const W: u32 = 16000;
        const H: u32 = 16000;
        let started = Instant::now();
        let pool = ResidentBudget::new(256 * 1024 * 1024).unwrap();
        let disk = DiskBudget::new(6 * 1024 * 1024 * 1024, 8).unwrap();
        let peak = Cell::new(0usize);
        let observe = || {
            peak.set(peak.get().max(pool.used()));
            Ok(())
        };
        let base = std::fs::canonicalize(std::env::temp_dir())
            .unwrap()
            .join(format!("peerbrush-selection-dense-{}", crate::engine::id()));
        std::fs::create_dir(&base).unwrap();
        let input_path = base.join("input");
        let output_path = base.join("output");
        let limits = Limits {
            disk_bytes: 4 * 1024 * 1024 * 1024,
            max_blocks: 8192,
            max_resident_blocks: 512,
            manifest_bytes: 16 * 1024 * 1024,
        };
        let input = Store::create_with_disk_budget(&input_path, pool.clone(), disk.clone(), limits)
            .unwrap();
        let spec = TileSpec::new(256, 256, 16).unwrap();
        let mut blocks = BTreeMap::new();
        let mut generated = Context::new(&SHA256);
        generated.update(b"PeerBrush-dense-selection-native-tiles-v1\0");
        for ty in 0..H.div_ceil(TILE) {
            for tx in 0..W.div_ceil(TILE) {
                let mut tile = ScratchBytes::new(&pool, spec.bytes(), spec.bytes()).unwrap();
                for y in 0..TILE {
                    for x in 0..TILE {
                        let gx = tx * TILE + x;
                        let gy = ty * TILE + y;
                        let pixel = if gx < W && gy < H {
                            native(gx, gy)
                        } else {
                            [0; 4]
                        };
                        let at = ((y * TILE + x) * 8) as usize;
                        for (channel, bytes) in
                            pixel.into_iter().zip(tile[at..at + 8].chunks_exact_mut(2))
                        {
                            bytes.copy_from_slice(&channel.to_be_bytes());
                        }
                    }
                }
                generated.update(&tx.to_be_bytes());
                generated.update(&ty.to_be_bytes());
                generated.update(&tile);
                blocks.insert((tx, ty), input.put(spec, &tile, &mut || observe()).unwrap());
            }
        }
        let generated = finish(generated);
        let refs: Vec<_> = blocks.values().copied().collect();
        let snapshot = input
            .commit(
                [37; 16],
                1,
                None,
                b"DenseNative16SelectionInputV1",
                &refs,
                &mut || observe(),
            )
            .unwrap();
        let selected = snapshot.reference();
        drop(snapshot);
        drop(input);
        let input_file = input_path.join("blocks.pbt");
        let input_bytes = std::fs::metadata(&input_file).unwrap().len();
        #[cfg(windows)]
        {
            use std::os::windows::fs::MetadataExt;
            assert_eq!(
                std::fs::metadata(&input_file).unwrap().file_attributes() & (0x200 | 0x800),
                0,
                "Dense source must not be a sparse or filesystem-compressed fixture"
            );
        }
        assert!(
            input_bytes > 2_000_000_000,
            "Source must be actual unique dense bytes, not sparse/deduplicated input"
        );
        let file_before = file_hash(&input_file);
        let snapshot = Store::recover_with_disk_budget(
            &input_path,
            selected,
            pool.clone(),
            disk.clone(),
            limits,
            &mut || observe(),
        )
        .unwrap();
        let input = snapshot.store();
        drop(snapshot);
        let source =
            Surface::from_plane(Plane::from_blocks(W, H, 16, input.clone(), blocks).unwrap())
                .unwrap();
        assert!(source.uses_disk_budget(&disk));
        assert!(source.uses_budget(&pool));
        let native_before = native_hash(&source);
        assert_eq!(native_before, generated);
        let staged_seconds = started.elapsed().as_secs_f64();
        let output =
            Store::create_with_disk_budget(&output_path, pool.clone(), disk.clone(), limits)
                .unwrap();
        let request = Request {
            width: W,
            height: H,
            seeds: &[[0, 0]],
            quick_radius: None,
            feather: 0,
            mode: Combine::Replace,
            maximum_work: 256_000_000,
        };
        let mut candidate = None;
        let mut cancel_samples = 0u64;
        let mut selected_samples = 0u64;
        let selection_started = Instant::now();
        for cancel in [true, false] {
            let cache = RefCell::new(Vec::<(u32, SurfaceRegion)>::new());
            let samples = Cell::new(0u64);
            let check = || {
                observe()?;
                if cancel && samples.get() >= 100_000 {
                    Err(Error::Cancelled)
                } else {
                    Ok(())
                }
            };
            let result = prepare(
                &request,
                output.clone(),
                None,
                &check,
                |y| {
                    let tile_row = y / TILE;
                    let mut cache = cache.borrow_mut();
                    let found = cache.iter().position(|(key, _)| *key == tile_row);
                    if found.is_some_and(|index| {
                        let b = cache[index].1.region().bounds();
                        y as i32 >= b[1] && (y as i32) < b[3]
                    }) {
                        return Ok(());
                    }
                    if found.is_none() && cache.len() == 2 {
                        cache.remove(0);
                    }
                    let top = y / 8 * 8;
                    let view = source.prepare_region(
                        Region::new([0, top as i32, W as i32, (top + 8).min(H) as i32])?,
                        &mut || check(),
                    )?;
                    if let Some(index) = found {
                        cache[index] = (tile_row, view);
                    } else {
                        cache.push((tile_row, view));
                    }
                    Ok(())
                },
                |_, x, y| {
                    let cache = cache.borrow();
                    let view = &cache
                        .iter()
                        .find(|(_, view)| {
                            let b = view.region().bounds();
                            (y as i32) >= b[1] && (y as i32) < b[3]
                        })
                        .ok_or(Error::Invalid("Dense native row was not prepared"))?
                        .1;
                    let pixel = view.pixel16(x as i32, y as i32)?;
                    if pixel != native(x, y) {
                        return Err(Error::Corrupt);
                    }
                    samples.set(samples.get() + 1);
                    Ok(true)
                },
            );
            if cancel {
                assert!(result.is_err());
                cancel_samples = samples.get();
                assert!(cancel_samples >= 100_000);
            } else {
                candidate = Some(result.unwrap());
                selected_samples = samples.get();
            }
            drop(cache);
            if cancel {
                assert_eq!(pool.used(), 0);
            }
        }
        let selection_seconds = selection_started.elapsed().as_secs_f64();
        assert_eq!(selected_samples, u64::from(W) * u64::from(H));
        let candidate = candidate.unwrap();
        assert_eq!(candidate.bounds(), [0, 0, W as i32, H as i32]);
        assert_eq!(candidate.origin(), [0, 0]);
        assert_eq!(candidate.contours().len(), 1);
        assert_eq!(candidate.contours()[0].len(), 4);
        let mask_bounds = candidate.bounds();
        let mask_contours = candidate.contours().to_vec();
        let mut corners: Vec<_> = mask_contours[0]
            .iter()
            .map(|point| [point[0] as u32, point[1] as u32])
            .collect();
        corners.sort();
        let mut expected = vec![[0, 0], [W, 0], [W, H], [0, H]];
        expected.sort();
        assert_eq!(corners, expected);
        assert!(candidate.mask().uses_disk_budget(&disk));
        let mut mask_hash = Context::new(&SHA256);
        for ty in 0..H.div_ceil(TILE) {
            for tx in 0..W.div_ceil(TILE) {
                let tile = candidate
                    .mask()
                    .prepare_tile((tx, ty), &mut || observe())
                    .unwrap();
                let bytes = tile.bytes().expect("Full selection cannot be sparse");
                mask_hash.update(bytes);
                for y in 0..TILE {
                    for x in 0..TILE {
                        let at = ((y * TILE + x) * 4) as usize;
                        assert_eq!(
                            &bytes[at..at + 4],
                            if tx * TILE + x < W && ty * TILE + y < H {
                                &[255; 4]
                            } else {
                                &[0; 4]
                            }
                        );
                    }
                }
            }
        }
        let native_after = native_hash(&source);
        assert_eq!(native_before, native_after);
        let output_file = output_path.join("blocks.pbt");
        let output_bytes = std::fs::metadata(&output_file).unwrap().len();
        let disk_bytes = disk.used().unwrap();
        assert_eq!(disk_bytes, input_bytes + output_bytes);
        assert!(peak.get() <= pool.limit());
        drop(candidate);
        drop(source);
        drop(input);
        drop(output);
        assert_eq!(pool.used(), 0);
        let file_after = file_hash(&input_file);
        assert_eq!(file_before, file_after);
        let receipt = serde_json::json!({"scope":"Actual dense native16 Surface selection candidate component only; no editor, styled composite, Save/history, installed or performance qualification", "width":W,"height":H,"native_depth":16,"source_file_bytes":input_bytes,"source_blocks":refs.len(),"source_file_sha256_before":hex(file_before),"source_file_sha256_after":hex(file_after),"full_native_tile_sha256_before":hex(native_before),"full_native_tile_sha256_after":hex(native_after),"mask_native_sha256":hex(finish(mask_hash)),"mask_bounds":mask_bounds,"mask_contours":mask_contours,"selected_samples":selected_samples,"cancelled_after_samples":cancel_samples,"resident_ledger_peak_bytes":peak.get(),"resident_ledger_limit_bytes":pool.limit(),"resident_after_close":pool.used(),"disk_ledger_bytes":disk_bytes,"disk_limit_bytes":disk.limit(),"output_file_bytes":output_bytes,"staging_seconds":staged_seconds,"selection_including_cancel_seconds":selection_seconds,"total_seconds":started.elapsed().as_secs_f64(),"exclusions":"Fixture/index/allocator metadata and OS caches are not total process RAM; shared ledger peak is admitted payload accounting. Private owned files retained for independent review."});
        std::fs::write(
            base.join("receipt.json"),
            serde_json::to_vec_pretty(&receipt).unwrap(),
        )
        .unwrap();
        println!(
            "DENSE_SELECTION_RECEIPT {}",
            base.join("receipt.json").display()
        );
        println!("{}", receipt);
    }
}
