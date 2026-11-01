//! Immutable selection coverage authority. Source staging and preparation are
//! explicit; no source I/O or implicit empty coverage occurs in sampling.
use crate::{
    large_document::{Error, ResidentBudget, Result, Store},
    raster::{
        backed::Region,
        surface::{Surface, SurfaceKey, SurfaceRegion},
    },
};
use std::sync::Arc;

#[derive(Clone, Debug)]
pub struct CoverageSource {
    mask: Surface,
    bounds: [i32; 4],
    origin: [i32; 2],
    contours: Arc<Vec<Vec<[f32; 2]>>>,
}

impl CoverageSource {
    /// Codec/worker source proofs and document admission remain separate. The
    /// source is native scalar8, independent of the artwork's native depth.
    pub fn new(
        mask: Surface,
        bounds: [i32; 4],
        origin: [i32; 2],
        contours: Arc<Vec<Vec<[f32; 2]>>>,
    ) -> Result<Self> {
        if mask.depth() != 8
            || origin
                .iter()
                .chain(bounds.iter())
                .any(|v| v.unsigned_abs() > 100000)
            || bounds[0] > bounds[2]
            || bounds[1] > bounds[3]
            || contours
                .iter()
                .try_fold(0usize, |n, line| n.checked_add(line.len()))
                .is_none_or(|n| n > 500000)
            || contours
                .iter()
                .flatten()
                .flatten()
                .any(|v| !v.is_finite() || v.abs() > 100000.)
        {
            return Err(Error::Invalid("Invalid native selection source metadata"));
        }
        Ok(Self {
            mask,
            bounds,
            origin,
            contours,
        })
    }

    pub fn stage_legacy(
        source: &super::Coverage,
        store: Store,
        check: &mut dyn FnMut() -> Result<()>,
    ) -> Result<Self> {
        source
            .validate()
            .map_err(|_| Error::Invalid("Invalid legacy selection coverage"))?;
        // Whole-raster staging refuses retained originals until the scene's
        // explicit retained-source collector can carry them without loss.
        let mask = Surface::stage_raster(&source.mask, store, check)?;
        Self::new(
            mask,
            source.bounds,
            source.origin,
            Arc::new(source.contours.clone()),
        )
    }

    pub fn mask(&self) -> &Surface {
        &self.mask
    }
    pub fn key(&self) -> SurfaceKey {
        self.mask.key()
    }
    pub fn bounds(&self) -> [i32; 4] {
        self.bounds
    }
    pub fn origin(&self) -> [i32; 2] {
        self.origin
    }
    pub fn contours(&self) -> &Arc<Vec<Vec<[f32; 2]>>> {
        &self.contours
    }

    /// `region` is in document coordinates. Only its exact physical mask
    /// intersection is pinned; metadata-proven exterior coverage is zero.
    pub fn prepare_region(
        &self,
        region: Region,
        budget: &ResidentBudget,
        check: &mut dyn FnMut() -> Result<()>,
    ) -> Result<PreparedCoverageRegion> {
        check()?;
        if !self.mask.uses_budget(budget) {
            return Err(Error::Invalid(
                "Selection source uses a different payload ledger",
            ));
        }
        let [left, top, right, bottom] = region.bounds();
        let x0 = (i64::from(left) - i64::from(self.origin[0])).max(0);
        let y0 = (i64::from(top) - i64::from(self.origin[1])).max(0);
        let x1 = (i64::from(right) - i64::from(self.origin[0])).min(i64::from(self.mask.width()));
        let y1 = (i64::from(bottom) - i64::from(self.origin[1])).min(i64::from(self.mask.height()));
        let pixels = if x0 < x1 && y0 < y1 {
            Some(self.mask.prepare_region(
                Region::new([x0 as i32, y0 as i32, x1 as i32, y1 as i32])?,
                check,
            )?)
        } else {
            None
        };
        check()?;
        Ok(PreparedCoverageRegion {
            pixels,
            source: self.clone(),
            region,
        })
    }
}

pub struct PreparedCoverageRegion {
    // Release native payload pins before the source owner/file handles.
    pixels: Option<SurfaceRegion>,
    source: CoverageSource,
    region: Region,
}

impl PreparedCoverageRegion {
    pub fn key(&self) -> SurfaceKey {
        self.source.key()
    }
    pub fn region(&self) -> Region {
        self.region
    }
    pub fn value(&self, x: i32, y: i32) -> Result<f32> {
        let [left, top, right, bottom] = self.region.bounds();
        if x < left || x >= right || y < top || y >= bottom {
            return Err(Error::Invalid(
                "Selection sample is outside its prepared footprint",
            ));
        }
        let local = [
            i64::from(x) - i64::from(self.source.origin[0]),
            i64::from(y) - i64::from(self.source.origin[1]),
        ];
        if local[0] < 0
            || local[1] < 0
            || local[0] >= i64::from(self.source.mask.width())
            || local[1] >= i64::from(self.source.mask.height())
        {
            return Ok(0.);
        }
        let pixels = self.pixels.as_ref().ok_or(Error::Invalid(
            "Selection source footprint was not prepared",
        ))?;
        // Coverage is scalar red, not premultiplied RGBA or independent alpha.
        Ok(f32::from(pixels.pixel(local[0] as i32, local[1] as i32)?[0]) / 255.)
    }
    pub fn copy_row(&self, y: i32, output: &mut [u8]) -> Result<()> {
        let [left, top, right, bottom] = self.region.bounds();
        if y < top || y >= bottom || output.len() != (right - left) as usize {
            return Err(Error::Invalid(
                "Selection row differs from its prepared footprint",
            ));
        }
        for (x, value) in (left..right).zip(output) {
            *value = (self.value(x, y)? * 255.).round() as u8;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{large_document::Limits, raster::Raster};
    fn store(pool: ResidentBudget) -> (Store, std::path::PathBuf) {
        let path =
            std::env::temp_dir().join(format!("peerbrush-coverage-source-{}", crate::engine::id()));
        let store = Store::create(
            &path,
            pool,
            Limits {
                disk_bytes: 16 * 1024 * 1024,
                max_blocks: 32,
                max_resident_blocks: 16,
                manifest_bytes: 1024 * 1024,
            },
        )
        .unwrap();
        (store, path)
    }
    fn legacy() -> super::super::Coverage {
        let mut mask = Raster::new(4, 2);
        for y in 0..2 {
            for x in 0..4 {
                mask.set(x, y, [(31 + x * 17 + y * 9) as u8, 253, 17, 0]);
            }
        }
        super::super::Coverage {
            mask,
            bounds: [3, 1, 7, 3],
            origin: [3, 1],
            contours: vec![vec![[3., 1.], [7., 1.], [7., 3.], [3., 3.]]],
        }
    }
    #[test]
    fn prepared_coverage_keeps_scalar_red_origin_metadata_source_owners_and_charges() {
        let pool = ResidentBudget::new(4 * 1024 * 1024).unwrap();
        let (store, path) = store(pool.clone());
        let original = legacy();
        let source =
            CoverageSource::stage_legacy(&original, store.clone(), &mut || Ok(())).unwrap();
        assert_eq!(source.bounds(), original.bounds);
        assert_eq!(source.origin(), original.origin);
        assert_eq!(&**source.contours(), &original.contours);
        let prepared = source
            .prepare_region(Region::new([0, 0, 8, 4]).unwrap(), &pool, &mut || Ok(()))
            .unwrap();
        assert_eq!(prepared.key(), source.key());
        assert_eq!(pool.used(), 256 * 256 * 4);
        for y in 0..4 {
            for x in 0..8 {
                assert_eq!(prepared.value(x, y).unwrap(), original.value(x, y));
            }
        }
        assert!(prepared.value(8, 1).is_err());
        let mut row = vec![0; 8];
        prepared.copy_row(1, &mut row).unwrap();
        assert_eq!(row[3], 31);
        assert!(prepared.copy_row(1, &mut row[..2]).is_err());
        drop(source);
        drop(store);
        assert_eq!(pool.used(), 256 * 256 * 4);
        assert_eq!(prepared.value(3, 1).unwrap(), 31. / 255.);
        drop(prepared);
        assert_eq!(pool.used(), 0);
        std::fs::remove_dir_all(path).unwrap();
    }
    #[test]
    fn wrong_pool_budget_and_cancel_never_become_empty_coverage() {
        for cancel in [false, true] {
            let pool =
                ResidentBudget::new(if cancel { 4 * 1024 * 1024 } else { 128 * 1024 }).unwrap();
            let (store, path) = store(pool.clone());
            let source =
                CoverageSource::stage_legacy(&legacy(), store.clone(), &mut || Ok(())).unwrap();
            assert!(source
                .prepare_region(
                    Region::new([3, 1, 7, 3]).unwrap(),
                    &ResidentBudget::new(pool.limit()).unwrap(),
                    &mut || Ok(())
                )
                .is_err());
            let result =
                source.prepare_region(Region::new([3, 1, 7, 3]).unwrap(), &pool, &mut || {
                    if cancel && pool.used() > 0 {
                        Err(Error::Cancelled)
                    } else {
                        Ok(())
                    }
                });
            assert!(matches!(
                (cancel, result),
                (false, Err(Error::Budget)) | (true, Err(Error::Cancelled))
            ));
            assert_eq!(pool.used(), 0);
            drop(source);
            drop(store);
            std::fs::remove_dir_all(path).unwrap();
        }
    }
    #[test]
    fn metadata_proven_off_canvas_sources_still_enforce_declared_sampling_footprint() {
        let pool = ResidentBudget::new(1).unwrap();
        let source = CoverageSource::new(
            Surface::zero(16000, 16000, 8).unwrap(),
            [0; 4],
            [-100000, -100000],
            Arc::new(vec![]),
        )
        .unwrap();
        let view = source
            .prepare_region(
                Region::new([0, 0, 16000, 8]).unwrap(),
                &pool,
                &mut || Ok(()),
            )
            .unwrap();
        assert_eq!(view.value(15999, 7).unwrap(), 0.);
        assert!(view.value(16000, 7).is_err());
        assert_eq!(pool.used(), 0);
        assert!(CoverageSource::new(
            Surface::zero(1, 1, 16).unwrap(),
            [0; 4],
            [0; 2],
            Arc::new(vec![])
        )
        .is_err());
    }

    #[test]
    fn legacy_retained_mask_authority_is_refused_rather_than_silently_dropped() {
        let pool = ResidentBudget::new(4 * 1024 * 1024).unwrap();
        let (store, path) = store(pool.clone());
        let mut original = legacy();
        let retained = crate::retained::Original::from(&original.mask);
        original.mask.retained = Some(Box::new(retained));
        assert!(CoverageSource::stage_legacy(&original, store.clone(), &mut || Ok(())).is_err());
        assert!(original.mask.retained.is_some());
        assert_eq!(pool.used(), 0);
        drop(store);
        std::fs::remove_dir_all(path).unwrap();
    }
}
