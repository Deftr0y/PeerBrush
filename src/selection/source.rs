//! Selection geometry and native coverage share explicit regional sampling.
use super::backed::{CoverageSource, PreparedCoverageRegion};
use crate::{
    large_document::{Error, ResidentBudget, Result},
    raster::backed::Region,
};
use std::sync::Arc;

#[derive(Clone, Debug)]
pub enum SelectionSource {
    All,
    Geometry {
        bounds: [i32; 4],
        polygon: Option<Arc<Vec<[f32; 2]>>>,
    },
    Coverage(CoverageSource),
}

impl SelectionSource {
    /// Match the existing priority: matching coverage, otherwise no restriction
    /// or the rectangle and its matching polygon. Stale geometry cannot clip it.
    pub fn from_metadata(
        selection: Option<[i32; 4]>,
        polygon: Option<Arc<Vec<[f32; 2]>>>,
        coverage: Option<CoverageSource>,
    ) -> Result<Self> {
        if let Some(coverage) = coverage {
            if selection == Some(coverage.bounds()) {
                return Ok(Self::Coverage(coverage));
            }
        }
        let Some(bounds) = selection else {
            return Ok(Self::All);
        };
        if bounds.iter().any(|v| v.unsigned_abs() > 100000)
            || bounds[0] > bounds[2]
            || bounds[1] > bounds[3]
        {
            return Err(Error::Invalid("Invalid selection geometry source"));
        }
        let polygon = polygon.filter(|points| super::bounds(points).ok() == Some(bounds));
        Ok(Self::Geometry { bounds, polygon })
    }

    pub fn prepare_region(
        &self,
        region: Region,
        budget: &ResidentBudget,
        check: &mut dyn FnMut() -> Result<()>,
    ) -> Result<PreparedSelection> {
        check()?;
        let kind = match self {
            Self::All => PreparedKind::All,
            Self::Geometry { bounds, polygon } => PreparedKind::Geometry {
                bounds: *bounds,
                polygon: polygon.clone(),
            },
            Self::Coverage(coverage) => {
                PreparedKind::Coverage(coverage.prepare_region(region, budget, check)?)
            }
        };
        check()?;
        Ok(PreparedSelection { region, kind })
    }
}

pub struct PreparedSelection {
    region: Region,
    kind: PreparedKind,
}
enum PreparedKind {
    All,
    Geometry {
        bounds: [i32; 4],
        polygon: Option<Arc<Vec<[f32; 2]>>>,
    },
    Coverage(PreparedCoverageRegion),
}
impl PreparedSelection {
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
        Ok(match &self.kind {
            PreparedKind::All => 1.,
            PreparedKind::Geometry { bounds, polygon } => f32::from(u8::from(
                x >= bounds[0]
                    && x < bounds[2]
                    && y >= bounds[1]
                    && y < bounds[3]
                    && polygon.as_ref().is_none_or(|points| {
                        super::contains(points, x as f32 + 0.5, y as f32 + 0.5)
                    }),
            )),
            PreparedKind::Coverage(coverage) => return coverage.value(x, y),
        })
    }
    pub fn contains(&self, x: i32, y: i32) -> Result<bool> {
        Ok(self.value(x, y)? > 0.)
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
    #[test]
    fn pure_geometry_matches_legacy_pixel_centers_priority_and_stale_polygon_rules() {
        let pool = ResidentBudget::new(1).unwrap();
        let region = Region::new([-2, -2, 9, 9]).unwrap();
        let triangle = Arc::new(vec![[0., 0.], [8., 0.], [0., 8.]]);
        let stale = Arc::new(vec![[1., 1.], [7., 1.], [7., 7.], [1., 7.]]);
        let zero = CoverageSource::new(
            crate::raster::surface::Surface::zero(8, 8, 8).unwrap(),
            [0, 0, 8, 8],
            [0; 2],
            &[],
            &pool,
        )
        .unwrap();
        for (case, source, expected_count) in [
            (
                0,
                SelectionSource::from_metadata(None, Some(stale.clone()), Some(zero.clone()))
                    .unwrap(),
                121,
            ),
            (
                1,
                SelectionSource::from_metadata(Some([0, 0, 8, 8]), Some(triangle), None).unwrap(),
                36,
            ),
            (
                2,
                SelectionSource::from_metadata(Some([0, 0, 8, 8]), Some(stale.clone()), None)
                    .unwrap(),
                64,
            ),
            (
                3,
                SelectionSource::from_metadata(Some([0, 0, 8, 8]), Some(stale), Some(zero))
                    .unwrap(),
                0,
            ),
        ] {
            let prepared = source
                .prepare_region(region, &pool, &mut || Ok(()))
                .unwrap();
            let mut selected = 0;
            for y in -2..9 {
                for x in -2..9 {
                    let expected = match case {
                        0 => true,
                        1 => x >= 0 && y >= 0 && x + y <= 7,
                        2 => (0..8).contains(&x) && (0..8).contains(&y),
                        _ => false,
                    };
                    assert_eq!(prepared.contains(x, y).unwrap(), expected);
                    selected += usize::from(expected);
                }
            }
            assert_eq!(selected, expected_count);
            assert!(prepared.value(9, 0).is_err());
        }
        assert_eq!(pool.used(), 0);
        let source = SelectionSource::All;
        assert!(source
            .prepare_region(region, &pool, &mut || Err(Error::Cancelled))
            .is_err());
        fn threadsafe<T: Send + Sync>() {}
        threadsafe::<SelectionSource>();
        threadsafe::<PreparedSelection>();
    }
}
