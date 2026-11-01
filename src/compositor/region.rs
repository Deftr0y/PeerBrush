//! Explicit source footprints for a regional scene. Preparation does all I/O;
//! sampling an undeclared footprint is an error, including for known sources.
use crate::{
    large_document::{Error, Reservation, ResidentBudget, Result},
    raster::{
        backed::Region,
        surface::{Surface, SurfaceKey, SurfaceRegion},
        Pixel, Pixel16,
    },
};

const MAX_FOOTPRINTS: usize = 4096;

pub(crate) struct PreparedSceneRegion {
    footprints: Vec<SurfaceRegion>,
    budget: ResidentBudget,
    _index: Reservation,
}

impl PreparedSceneRegion {
    #[cfg(test)]
    fn index_bytes(&self) -> usize {
        self._index.bytes()
            + self
                .footprints
                .iter()
                .map(SurfaceRegion::index_bytes)
                .sum::<usize>()
    }
    /// Call outside engine, registry and UI locks. The caller's check retains
    /// the scene's source/revision/cancellation admission until final adoption.
    /// All source pins must share the caller's actual global payload ledger.
    pub(crate) fn prepare(
        requests: &[(&Surface, Region)],
        budget: &ResidentBudget,
        check: &mut dyn FnMut() -> Result<()>,
    ) -> Result<Self> {
        if requests.len() > MAX_FOOTPRINTS {
            return Err(Error::Budget);
        }
        check()?;
        if requests
            .iter()
            .any(|(surface, _)| !surface.uses_budget(budget))
        {
            return Err(Error::Invalid(
                "Scene sources use a different payload ledger",
            ));
        }
        let bytes = requests
            .len()
            .checked_mul(std::mem::size_of::<SurfaceRegion>())
            .and_then(|n| n.checked_add(64))
            .ok_or(Error::Budget)?;
        let charge = budget.reserve_bytes(bytes, 4 * 1024 * 1024)?;
        let mut footprints = Vec::new();
        footprints
            .try_reserve_exact(requests.len())
            .map_err(|_| Error::Budget)?;
        for &(surface, region) in requests {
            check()?;
            let pinned = surface.prepare_region(region, check)?;
            if pinned.key() != surface.key() || pinned.region() != region {
                return Err(Error::Changed);
            }
            footprints.push(pinned);
        }
        check()?;
        footprints.sort_unstable_by_key(|view| view.key());
        Ok(Self {
            footprints,
            budget: budget.clone(),
            _index: charge,
        })
    }

    /// Resolve once when compiling a layer's regional composition plan, rather
    /// than searching the entire scene for each pixel.
    pub(crate) fn source(&self, key: SurfaceKey) -> Result<PreparedSurface<'_>> {
        let first = self.footprints.partition_point(|view| view.key() < key);
        let end = self.footprints.partition_point(|view| view.key() <= key);
        if first == end {
            return Err(Error::Invalid("Scene source has no prepared footprints"));
        }
        Ok(PreparedSurface {
            regions: &self.footprints[first..end],
        })
    }

    /// The provisional output uses the same ledger as all source pins. A
    /// failed sampler or final check cannot publish a partially rendered frame.
    pub(crate) fn render_native(
        &self,
        region: Region,
        depth: u16,
        check: &mut dyn FnMut() -> std::result::Result<(), String>,
        sample: impl FnMut(i32, i32) -> std::result::Result<Pixel16, String>,
    ) -> std::result::Result<crate::large_document::PinnedBytes, String> {
        crate::render::region::native(region, depth, &self.budget, check, sample)
    }
}

pub(crate) struct PreparedSurface<'a> {
    regions: &'a [SurfaceRegion],
}

impl PreparedSurface<'_> {
    fn footprint(&self, x: i32, y: i32) -> Result<&SurfaceRegion> {
        self.regions
            .iter()
            .find(|view| {
                let [left, top, right, bottom] = view.region().bounds();
                x >= left && y >= top && x < right && y < bottom
            })
            .ok_or(Error::Invalid(
                "Scene sample is outside its prepared source footprints",
            ))
    }

    pub(crate) fn pixel16(&self, x: i32, y: i32) -> Result<Pixel16> {
        self.footprint(x, y)?.pixel16(x, y)
    }

    pub(crate) fn pixel(&self, x: i32, y: i32) -> Result<Pixel> {
        self.footprint(x, y)?.pixel(x, y)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        large_document::{Limits, Store},
        raster::Raster,
    };

    fn store(budget: &ResidentBudget) -> (Store, std::path::PathBuf) {
        let parent = std::fs::canonicalize(std::env::temp_dir()).unwrap();
        let path = parent.join(format!("pb-scene-region-{}", crate::engine::id()));
        let store = Store::create(
            &path,
            budget.clone(),
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

    fn cleanup(path: &std::path::Path) {
        let owned = std::fs::canonicalize(path).unwrap();
        assert_eq!(
            owned.parent().unwrap(),
            std::fs::canonicalize(std::env::temp_dir()).unwrap()
        );
        assert!(owned
            .file_name()
            .unwrap()
            .to_string_lossy()
            .starts_with("pb-scene-region-"));
        std::fs::remove_dir_all(owned).unwrap();
    }

    #[test]
    fn native_sources_resolve_once_and_never_sample_an_unprepared_gap() {
        let budget = ResidentBudget::new(4 * 1024 * 1024).unwrap();
        let (store, path) = store(&budget);
        let mut pixels = Raster::new_depth(257, 2, 16);
        pixels.set16(0, 0, [12347, 33559, 51237, 65535]);
        pixels.set16(256, 0, [65535, 0, 7, 30001]);
        let surface = Surface::stage_raster(&pixels, store.clone(), &mut || Ok(())).unwrap();
        let zero = Surface::zero(19, 23, 8).unwrap();
        let requests = [
            (&surface, Region::new([256, 0, 257, 1]).unwrap()),
            (&surface, Region::new([0, 0, 2, 1]).unwrap()),
        ];
        // Warm only the retained weak-cache metadata. All payload pins and ROI
        // containers drop before taking the persistent-owner baseline.
        drop(PreparedSceneRegion::prepare(&requests, &budget, &mut || Ok(())).unwrap());
        let baseline = budget.used();
        let scene = PreparedSceneRegion::prepare(&requests, &budget, &mut || Ok(())).unwrap();
        let source = scene.source(surface.key()).unwrap();
        assert_eq!(source.pixel16(0, 0).unwrap(), pixels.get16(0, 0));
        assert_eq!(source.pixel16(256, 0).unwrap(), pixels.get16(256, 0));
        assert!(source.pixel16(2, 0).is_err());
        assert!(source.pixel16(0, 1).is_err());
        assert!(scene.source(zero.key()).is_err());
        let output = scene
            .render_native(
                Region::new([0, 0, 2, 1]).unwrap(),
                16,
                &mut || Ok(()),
                |x, y| source.pixel16(x, y).map_err(|error| error.to_string()),
            )
            .unwrap();
        assert_eq!(
            &output[..8],
            &[0x30, 0x3b, 0x83, 0x17, 0xc8, 0x25, 0xff, 0xff]
        );
        drop(output);
        drop(scene);
        assert_eq!(budget.used(), baseline);
        drop(surface);
        drop(store);
        assert_eq!(budget.used(), 0);
        cleanup(&path);
    }

    #[test]
    fn wrong_equal_sized_pool_and_pin_cancel_cannot_publish_scene_pins() {
        let budget = ResidentBudget::new(1024 * 1024).unwrap();
        let other = ResidentBudget::new(budget.limit()).unwrap();
        let (store, path) = store(&budget);
        let mut pixels = Raster::new_depth(16, 16, 8);
        pixels.set(0, 0, [17, 31, 53, 255]);
        let surface = Surface::stage_raster(&pixels, store.clone(), &mut || Ok(())).unwrap();
        let requests = [(&surface, Region::new([0, 0, 2, 1]).unwrap())];
        let initial = budget.used();
        assert!(PreparedSceneRegion::prepare(&requests, &other, &mut || Ok(())).is_err());
        assert_eq!(budget.used(), initial);
        drop(PreparedSceneRegion::prepare(&requests, &budget, &mut || Ok(())).unwrap());
        let baseline = budget.used();
        let mut saw_pin = false;
        let scene = PreparedSceneRegion::prepare(&requests, &budget, &mut || {
            if budget.used() >= baseline + 256 * 256 * 4 {
                saw_pin = true;
                Err(Error::Cancelled)
            } else {
                Ok(())
            }
        });
        assert!(saw_pin && matches!(scene, Err(Error::Cancelled)));
        assert_eq!(budget.used(), baseline);
        assert_eq!(other.used(), 0);
        drop(surface);
        drop(store);
        assert_eq!(budget.used(), 0);
        cleanup(&path);
    }

    #[test]
    fn final_check_after_complete_pinning_can_refuse_scene_publication() {
        let budget = ResidentBudget::new(1024 * 1024).unwrap();
        let (store, path) = store(&budget);
        let mut pixels = Raster::new_depth(16, 16, 8);
        pixels.set(0, 0, [17, 31, 53, 255]);
        let surface = Surface::stage_raster(&pixels, store.clone(), &mut || Ok(())).unwrap();
        let requests = [(&surface, Region::new([0, 0, 2, 1]).unwrap())];
        drop(PreparedSceneRegion::prepare(&requests, &budget, &mut || Ok(())).unwrap());
        let baseline = budget.used();
        let mut checks = 0;
        let complete = PreparedSceneRegion::prepare(&requests, &budget, &mut || {
            checks += 1;
            Ok(())
        })
        .unwrap();
        let descriptors = complete.index_bytes();
        assert_eq!(budget.used(), baseline + descriptors + 256 * 256 * 4);
        drop(complete);
        assert_eq!(budget.used(), baseline);
        let final_check = checks;
        checks = 0;
        let refused = PreparedSceneRegion::prepare(&requests, &budget, &mut || {
            checks += 1;
            if checks == final_check {
                assert_eq!(budget.used(), baseline + descriptors + 256 * 256 * 4);
                Err(Error::Cancelled)
            } else {
                Ok(())
            }
        });
        assert!(matches!(refused, Err(Error::Cancelled)));
        assert_eq!(checks, final_check);
        assert_eq!(budget.used(), baseline);
        drop(surface);
        drop(store);
        assert_eq!(budget.used(), 0);
        cleanup(&path);
    }

    #[test]
    fn declared_zero_border_is_distinct_from_missing_source_or_footprint() {
        let budget = ResidentBudget::new(1024).unwrap();
        let zero = Surface::zero(16000, 16000, 16).unwrap();
        let scene = PreparedSceneRegion::prepare(
            &[(&zero, Region::new([-1, -1, 1, 1]).unwrap())],
            &budget,
            &mut || Ok(()),
        )
        .unwrap();
        let source = scene.source(zero.key()).unwrap();
        assert_eq!(source.pixel(-1, -1).unwrap(), [0; 4]);
        assert_eq!(source.pixel16(0, 0).unwrap(), [0; 4]);
        assert!(source.pixel16(1, 0).is_err());
        assert_eq!(budget.used(), scene.index_bytes());
        drop(scene);
        assert_eq!(budget.used(), 0);
    }
    #[test]
    fn index_admission_refuses_before_source_preparation() {
        let budget = ResidentBudget::new(1).unwrap();
        let zero = Surface::zero(8, 8, 8).unwrap();
        let mut checks = 0;
        let refused = PreparedSceneRegion::prepare(
            &[(&zero, Region::new([0, 0, 1, 1]).unwrap())],
            &budget,
            &mut || {
                checks += 1;
                Ok(())
            },
        );
        assert!(matches!(refused, Err(Error::Budget)));
        assert_eq!(checks, 1);
        assert_eq!(budget.used(), 0);
    }
}
