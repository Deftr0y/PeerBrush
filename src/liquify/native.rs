//! Historic Liquify clips retain their physical native scalar8 source pins.
//! Selection summary bounds and contours never replace raw red-channel samples.
use super::regional::{Mapping, Plan, PreparedClip};
use crate::{
    effects::regional::Frame,
    engine::{
        native::{CoverageMetadata, EffectMetadata},
        scene::SceneSnapshot,
    },
    large_document::{Reservation, ResidentBudget},
    raster::{backed::Region, surface::SurfaceRegion},
};
type Result<T> = std::result::Result<T, String>;
type Check<'a> = &'a mut dyn FnMut() -> Result<()>;

struct Clip {
    views: Vec<SurfaceRegion>,
    origin: [i32; 2],
    size: [u32; 2],
    pool: ResidentBudget,
    _index: Reservation,
}
impl PreparedClip for Clip {
    fn uses_budget(&self, pool: &ResidentBudget) -> bool {
        self.pool.same_pool(pool)
    }
    fn value(&self, x: i32, y: i32) -> Result<f32> {
        let (x, y) = (
            i64::from(x) - i64::from(self.origin[0]),
            i64::from(y) - i64::from(self.origin[1]),
        );
        if x < 0 || y < 0 || x >= i64::from(self.size[0]) || y >= i64::from(self.size[1]) {
            return Ok(0.);
        }
        let index = self
            .views
            .partition_point(|view| view.region().bounds()[3] <= y as i32);
        let view = self
            .views
            .get(index)
            .ok_or("Historic Liquify clip has no prepared sample")?;
        Ok(f32::from(view.pixel(x as i32, y as i32).map_err(|e| e.to_string())?[0]) / 255.)
    }
}
fn physical(origin: [i32; 2], size: [u32; 2]) -> Result<[i32; 4]> {
    Ok([
        origin[0],
        origin[1],
        i32::try_from(i64::from(origin[0]) + i64::from(size[0]))
            .map_err(|_| "Liquify clip right overflow")?,
        i32::try_from(i64::from(origin[1]) + i64::from(size[1]))
            .map_err(|_| "Liquify clip bottom overflow")?,
    ])
}
fn clip(
    scene: &SceneSnapshot,
    coverage: CoverageMetadata<'_>,
    regions: &[Region],
    check: Check<'_>,
) -> Result<Box<dyn PreparedClip>> {
    let mask = coverage.mask();
    let source = scene.raster(mask.slot())?;
    if mask.depth() != 8
        || source.depth() != 8
        || !source.uses_budget(scene.budget())
        || !source.uses_disk_budget(scene.disk_budget())
    {
        return Err("Historic Liquify clip must retain its admitted native scalar8 source".into());
    }
    let bytes = regions
        .len()
        .checked_mul(512)
        .and_then(|v| v.checked_add(512))
        .ok_or("Liquify clip index overflow")?;
    let index = scene
        .budget()
        .reserve_bytes(bytes, bytes)
        .map_err(|e| e.to_string())?;
    let mut views = Vec::new();
    views
        .try_reserve_exact(regions.len())
        .map_err(|_| "Liquify clip index allocation refused")?;
    let origin = coverage.origin();
    for region in regions {
        check()?;
        let [l, t, r, b] = region.bounds();
        let bounds = [
            i64::from(l) - i64::from(origin[0]),
            i64::from(t) - i64::from(origin[1]),
            i64::from(r) - i64::from(origin[0]),
            i64::from(b) - i64::from(origin[1]),
        ];
        if bounds[0] < 0
            || bounds[1] < 0
            || bounds[2] > i64::from(mask.width())
            || bounds[3] > i64::from(mask.height())
        {
            return Err("Liquify clip request exceeds its physical source".into());
        }
        let local = Region::new(bounds.map(|v| v as i32)).map_err(|e| e.to_string())?;
        let mut interrupted = None;
        views.push(
            source
                .prepare_region(local, &mut || {
                    check().map_err(|error| {
                        interrupted = Some(error);
                        crate::large_document::Error::Cancelled
                    })
                })
                .map_err(|error| interrupted.unwrap_or_else(|| error.to_string()))?,
        );
    }
    check()?;
    Ok(Box::new(Clip {
        views,
        origin,
        size: [mask.width(), mask.height()],
        pool: scene.budget().clone(),
        _index: index,
    }))
}
pub(crate) fn prepare(
    scene: &SceneSnapshot,
    effect: EffectMetadata<'_>,
    frame: Frame,
    check: Check<'_>,
) -> Result<Mapping> {
    if effect.kind() != "liquify" {
        return Err("Liquify preparation requires its captured effect".into());
    }
    let plan = plan(frame, effect.settings(), effect.clips().map(|(stroke, coverage)| {
        let mask = coverage.mask();
        (stroke, coverage.origin(), [mask.width(), mask.height()])
    }), scene.budget(), check)?;
    let check = std::cell::RefCell::new(check);
    plan.prepare(&mut || (*check.borrow_mut())(), |stroke, regions| {
        let coverage = effect
            .clips()
            .find(|(index, _)| *index as usize == stroke)
            .ok_or("Missing captured Liquify clip")?
            .1;
        clip(scene, coverage, regions, &mut **check.borrow_mut())
    })
}

pub(crate) fn plan(
    frame: Frame,
    settings: &serde_json::Value,
    clips: impl Iterator<Item = (u32, [i32; 2], [u32; 2])>,
    pool: &ResidentBudget,
    check: Check<'_>,
) -> Result<Plan> {
    let count = settings["strokes"].as_array().map_or(0, Vec::len);
    if count > super::MAX_STROKES {
        return Err("Too many captured Liquify strokes".into());
    }
    let _index = pool.reserve_bytes(65536, 65536).map_err(|e| e.to_string())?;
    let mut extents = vec![None; count];
    for (stroke, origin, size) in clips {
        check()?;
        let slot = extents
            .get_mut(stroke as usize)
            .ok_or("Liquify clip stroke is outside the captured roster")?;
        if slot.is_some() {
            return Err("Duplicate captured Liquify clip".into());
        }
        *slot = Some(physical(origin, size)?);
    }
    Plan::new(frame, settings, &extents, pool, check)
}
