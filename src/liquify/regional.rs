//! Explicit, charged inverse mapping for native source windows. Historic soft
//! clips are prepared separately; their raster bytes never enter settings JSON.
use super::{Dab, Field, Mode, Stroke};
use crate::{
    effects::regional::Frame,
    large_document::{Reservation, ResidentBudget},
    raster::{
        backed::{Region, MAX_REGION_PIXELS},
        project16, Pixel16,
    },
};
use serde_json::Value;

type Check<'a> = &'a mut dyn FnMut() -> Result<(), String>;
const MAX_CLIP_PIXELS: usize = 16_000_000;

/// Implementations own all declared raw red-channel clip pins. Sampling is
/// memory-only and a missing declared sample is an error, never zero.
pub(crate) trait PreparedClip: Send + Sync {
    fn uses_budget(&self, pool: &ResidentBudget) -> bool;
    fn value(&self, x: i32, y: i32) -> Result<f32, String>;
}

pub(crate) struct Plan {
    geometry: super::Plan,
    frame: Frame,
    clips: Vec<Option<[i32; 4]>>,
    pool: ResidentBudget,
    // The geometry/metadata allocations drop before their charge.
    _metadata_charge: Reservation,
}
impl Plan {
    pub(crate) fn new(
        frame: Frame,
        settings: &Value,
        // Physical mask extents from origin + source dimensions. Selection
        // Coverage.bounds does not prove that exterior raw red values are zero.
        clips: &[Option<[i32; 4]>],
        pool: &ResidentBudget,
        check: Check<'_>,
    ) -> Result<Self, String> {
        check()?;
        let strokes = settings.get("strokes").and_then(Value::as_array);
        if clips.len() != strokes.map_or(0, Vec::len) || clips.len() > super::MAX_STROKES {
            return Err("Liquify typed clips do not match the ordered strokes".into());
        }
        let mut metadata = clips.len() * (std::mem::size_of::<Stroke>() + 64)
            + super::MAX_DABS * std::mem::size_of::<Dab>();
        for stroke in strokes.into_iter().flatten() {
            if !stroke["coverage"].is_null() {
                return Err(
                    "Regional Liquify requires typed clip sources, not inline raster JSON".into(),
                );
            }
            for name in ["points", "polygon"] {
                if let Some(points) = stroke.get(name).and_then(Value::as_array) {
                    metadata = metadata
                        .checked_add(
                            points
                                .len()
                                .checked_mul(8)
                                .ok_or("Liquify metadata size overflow")?,
                        )
                        .ok_or("Liquify metadata size overflow")?;
                }
            }
        }
        if metadata > 16 * 1024 * 1024 {
            return Err("Liquify geometry exceeds its metadata admission".into());
        }
        for bounds in clips.iter().flatten() {
            if bounds[0] >= bounds[2] || bounds[1] >= bounds[3] {
                return Err("Invalid Liquify typed clip bounds".into());
            }
        }
        let charge = pool
            .reserve_bytes(metadata, metadata)
            .map_err(|e| e.to_string())?;
        let [width, height] = frame.dimensions();
        let mut geometry = super::plan_strokes(width, height, super::parse(settings)?)?;
        // Equal rectangles/polygons do not prove equal historic clip sources.
        let active: std::collections::BTreeSet<_> =
            geometry.dabs.iter().map(|d| d.stroke).collect();
        geometry.unrestricted = active.iter().any(|&i| {
            geometry.strokes[i].selection.is_none()
                && geometry.strokes[i].polygon.is_none()
                && clips[i].is_none()
        });
        geometry.clips.clear();
        if !geometry.unrestricted {
            geometry
                .clips
                .try_reserve_exact(active.len())
                .map_err(|_| "Liquify clip allocation failed")?;
            geometry.clips.extend(active);
            let pixels = (geometry.bounds[2] - geometry.bounds[0]) as usize
                * (geometry.bounds[3] - geometry.bounds[1]) as usize;
            let cost: usize = geometry
                .clips
                .iter()
                .map(|&i| {
                    geometry.strokes[i]
                        .polygon
                        .as_ref()
                        .map_or(1, |p| p.len().max(1))
                })
                .sum();
            if pixels
                .checked_mul(cost)
                .is_none_or(|n| n > super::MAX_SELECTION_WORK)
            {
                return Err("Liquify typed clipping exceeds the bounded work budget".into());
            }
        }
        let mut clip_metadata = Vec::new();
        clip_metadata
            .try_reserve_exact(clips.len())
            .map_err(|_| "Liquify clip allocation failed")?;
        clip_metadata.extend_from_slice(clips);
        check()?;
        Ok(Self {
            geometry,
            frame,
            clips: clip_metadata,
            pool: pool.clone(),
            _metadata_charge: charge,
        })
    }

    /// Covers point and per-dab inverse clip probes. Physical clip bounds are
    /// metadata, while each returned region is an explicit source declaration.
    pub(crate) fn clip_regions(&self, stroke: usize) -> Result<Vec<Region>, String> {
        let clip = self
            .clips
            .get(stroke)
            .ok_or("Unknown Liquify clip stroke")?;
        let Some(clip) = clip else {
            return Ok(Vec::new());
        };
        if self.geometry.dabs.is_empty() {
            return Ok(Vec::new());
        }
        let padding = (self.geometry.strokes[stroke].radius * 2.).ceil() as i32 + 2;
        let b = self.geometry.bounds;
        let bounds = [
            (b[0] - padding).max(clip[0]),
            (b[1] - padding).max(clip[1]),
            (b[2] + padding).min(clip[2]),
            (b[3] + padding).min(clip[3]),
        ];
        regions(bounds, MAX_CLIP_PIXELS)
    }

    pub(crate) fn prepare(
        self,
        check: Check<'_>,
        mut prepare_clip: impl FnMut(usize, &[Region]) -> Result<Box<dyn PreparedClip>, String>,
    ) -> Result<Mapping, String> {
        check()?;
        let mut clips = Vec::new();
        clips
            .try_reserve_exact(self.clips.len())
            .map_err(|_| "Liquify clip owner allocation failed")?;
        for (index, bounds) in self.clips.iter().enumerate() {
            clips.push(if bounds.is_some() {
                let regions = self.clip_regions(index)?;
                let prepared = prepare_clip(index, &regions)?;
                if !prepared.uses_budget(&self.pool) {
                    return Err("Liquify clip belongs to another resident resource pool".into());
                }
                Some(prepared)
            } else {
                None
            });
        }
        let p = &self.geometry;
        let bytes = p
            .width
            .checked_mul(p.height)
            .and_then(|n| n.checked_mul(8))
            .ok_or("Liquify field allocation overflow")?;
        let mut charges = Vec::new();
        let mut left = bytes;
        while left != 0 {
            let next = left.min(64 * 1024 * 1024);
            charges.push(
                self.pool
                    .reserve_bytes(next, next)
                    .map_err(|e| e.to_string())?,
            );
            left -= next;
        }
        let mut values = Vec::new();
        values
            .try_reserve_exact(p.width * p.height)
            .map_err(|_| "Liquify field allocation failed")?;
        values.resize(p.width * p.height, [0.; 2]);
        let mut field = Field {
            values,
            origin: [p.bounds[0] as f32 + 0.5, p.bounds[1] as f32 + 0.5],
            step: p.step,
            width: p.width,
            height: p.height,
        };
        let change_bytes = p
            .changes
            .checked_mul(std::mem::size_of::<(usize, [f32; 2])>())
            .ok_or("Liquify change allocation overflow")?;
        let change_charge = self
            .pool
            .reserve_bytes(change_bytes, change_bytes)
            .map_err(|e| e.to_string())?;
        let mut changes = Vec::new();
        changes
            .try_reserve_exact(p.changes)
            .map_err(|_| "Liquify change allocation failed")?;
        for dab in &p.dabs {
            check()?;
            changes.clear();
            let stroke = &p.strokes[dab.stroke];
            let r = stroke.radius;
            let grid = |n: f32, o: f32| ((n - o) / field.step).floor() as i32;
            let left = grid(dab.center[0] - r, field.origin[0]).max(0);
            let top = grid(dab.center[1] - r, field.origin[1]).max(0);
            let right = (grid(dab.center[0] + r, field.origin[0]) + 1).min(field.width as i32 - 1);
            let bottom =
                (grid(dab.center[1] + r, field.origin[1]) + 1).min(field.height as i32 - 1);
            for y in top..=bottom {
                check()?;
                for x in left..=right {
                    let point = [
                        field.origin[0] + x as f32 * field.step,
                        field.origin[1] + y as f32 * field.step,
                    ];
                    let offset = [point[0] - dab.center[0], point[1] - dab.center[1]];
                    let q = (offset[0] * offset[0] + offset[1] * offset[1]) / (r * r);
                    if q >= 1. || !contains(stroke, clips[dab.stroke].as_deref(), point)? {
                        continue;
                    }
                    let falloff = (1. - q)
                        * (1. - q)
                        * dab.strength
                        * clip_value(clips[dab.stroke].as_deref(), point)?;
                    let index = y as usize * field.width + x as usize;
                    let displacement = if matches!(stroke.mode, Mode::Restore) {
                        let old = field.values[index];
                        [old[0] * (1. - falloff), old[1] * (1. - falloff)]
                    } else {
                        let delta = match stroke.mode {
                            Mode::Push => [-dab.delta[0] * falloff, -dab.delta[1] * falloff],
                            Mode::Expand => {
                                [-offset[0] * falloff * 0.15, -offset[1] * falloff * 0.15]
                            }
                            Mode::Pinch => [offset[0] * falloff * 0.15, offset[1] * falloff * 0.15],
                            Mode::Restore => unreachable!(),
                        };
                        let inverse = [point[0] + delta[0], point[1] + delta[1]];
                        if !contains(stroke, clips[dab.stroke].as_deref(), inverse)? {
                            continue;
                        }
                        let old = field.sample(inverse);
                        [delta[0] + old[0], delta[1] + old[1]]
                    };
                    if !displacement.iter().all(|v| v.is_finite()) {
                        return Err("Liquify mapping generated nonfinite coordinates".into());
                    }
                    if changes.len() == changes.capacity() {
                        return Err("Liquify changes exceed the admitted geometry".into());
                    }
                    changes.push((index, displacement));
                }
            }
            for &(index, displacement) in &changes {
                field.values[index] = displacement;
            }
        }
        check()?;
        drop(changes);
        drop(change_charge);
        Ok(Mapping {
            field,
            plan: self,
            clips,
            _charges: charges,
        })
    }
}

fn clip_value(clip: Option<&dyn PreparedClip>, point: [f32; 2]) -> Result<f32, String> {
    let v = match clip {
        Some(clip) => clip.value(point[0].floor() as i32, point[1].floor() as i32)?,
        None => 1.,
    };
    if !v.is_finite() || !(0. ..=1.).contains(&v) {
        return Err("Liquify prepared clip is not a finite coverage factor".into());
    }
    Ok(v)
}
fn contains(
    stroke: &Stroke,
    clip: Option<&dyn PreparedClip>,
    point: [f32; 2],
) -> Result<bool, String> {
    Ok(clip_value(clip, point)? > 0. && stroke.contains(point))
}

pub(crate) struct Mapping {
    field: Field,
    plan: Plan,
    clips: Vec<Option<Box<dyn PreparedClip>>>,
    _charges: Vec<Reservation>,
}
impl Mapping {
    pub(crate) fn validate_frame(&self, frame: Frame, pool: &ResidentBudget) -> Result<(), String> {
        if self.plan.frame.dimensions() != frame.dimensions() || !self.plan.pool.same_pool(pool) {
            return Err("Liquify mapping differs from its captured frame or resident pool".into());
        }
        Ok(())
    }
    pub(crate) fn source_position(&self, x: i32, y: i32) -> Result<Option<[f32; 2]>, String> {
        let [w, h] = self.plan.frame.dimensions();
        if x < 0 || y < 0 || x >= w as i32 || y >= h as i32 {
            return Err("Liquify output is outside the captured frame".into());
        }
        let point = [x as f32 + 0.5, y as f32 + 0.5];
        let d = self.field.sample(point);
        if d[0].abs() + d[1].abs() < 0.000001 {
            return Ok(None);
        }
        let mut admitted = self.plan.geometry.unrestricted;
        if !admitted {
            for &i in &self.plan.geometry.clips {
                if contains(
                    &self.plan.geometry.strokes[i],
                    self.clips[i].as_deref(),
                    point,
                )? {
                    admitted = true;
                    break;
                }
            }
        }
        Ok(admitted.then_some([x as f32 + d[0], y as f32 + d[1]]))
    }

    pub(crate) fn source_regions(
        &self,
        output: Region,
        check: Check<'_>,
    ) -> Result<Vec<Region>, String> {
        let [w, h] = self.plan.frame.dimensions();
        let b = output.bounds();
        if b[0] < 0 || b[1] < 0 || b[2] > w as i32 || b[3] > h as i32 {
            return Err("Liquify output footprint is outside the captured frame".into());
        }
        let mut bounds = [w as i32, h as i32, 0, 0];
        for y in b[1]..b[3] {
            check()?;
            for x in b[0]..b[2] {
                let (left, top, support) = match self.source_position(x, y)? {
                    Some(p) => (p[0].floor() as i32, p[1].floor() as i32, 2),
                    None => (x, y, 1),
                };
                if left < w as i32 && top < h as i32 && left + support > 0 && top + support > 0 {
                    bounds = [
                        bounds[0].min(left.max(0)),
                        bounds[1].min(top.max(0)),
                        bounds[2].max((left + support).min(w as i32)),
                        bounds[3].max((top + support).min(h as i32)),
                    ];
                }
            }
        }
        regions(bounds, MAX_CLIP_PIXELS)
    }

    pub(crate) fn sample(
        &self,
        x: i32,
        y: i32,
        depth: u16,
        mut source: impl FnMut(i32, i32) -> Result<Pixel16, String>,
    ) -> Result<Pixel16, String> {
        if ![8, 16].contains(&depth) {
            return Err("Invalid Liquify native depth".into());
        }
        let Some(p) = self.source_position(x, y)? else {
            return source(x, y);
        };
        let [w, h] = self.plan.frame.dimensions();
        let ix = p[0].floor() as i32;
        let iy = p[1].floor() as i32;
        let pixel =
            |x: i32, y: i32, source: &mut dyn FnMut(i32, i32) -> Result<Pixel16, String>| {
                if x < 0 || y < 0 || x >= w as i32 || y >= h as i32 {
                    Ok([0; 4])
                } else {
                    source(x, y)
                }
            };
        if depth == 8 {
            let fx = p[0] - ix as f32;
            let fy = p[1] - iy as f32;
            let mut color = [0f32; 3];
            let mut alpha = 0f32;
            for (dx, dy, weight) in [
                (0, 0, (1. - fx) * (1. - fy)),
                (1, 0, fx * (1. - fy)),
                (0, 1, (1. - fx) * fy),
                (1, 1, fx * fy),
            ] {
                let value = pixel(ix + dx, iy + dy, &mut source)?.map(project16);
                let a = f32::from(value[3]) * weight;
                alpha += a;
                for c in 0..3 {
                    color[c] += f32::from(value[c]) * a;
                }
            }
            if alpha < 0.00001 {
                return Ok([0; 4]);
            }
            Ok([
                (color[0] / alpha).round().clamp(0., 255.) as u16 * 257,
                (color[1] / alpha).round().clamp(0., 255.) as u16 * 257,
                (color[2] / alpha).round().clamp(0., 255.) as u16 * 257,
                alpha.round().clamp(0., 255.) as u16 * 257,
            ])
        } else {
            let fx = f64::from(p[0]) - f64::from(ix);
            let fy = f64::from(p[1]) - f64::from(iy);
            let mut color = [0f64; 3];
            let mut alpha = 0f64;
            for (dx, dy, weight) in [
                (0, 0, (1. - fx) * (1. - fy)),
                (1, 0, fx * (1. - fy)),
                (0, 1, (1. - fx) * fy),
                (1, 1, fx * fy),
            ] {
                let value = pixel(ix + dx, iy + dy, &mut source)?;
                let a = f64::from(value[3]) * weight;
                alpha += a;
                for c in 0..3 {
                    color[c] += f64::from(value[c]) * a;
                }
            }
            if alpha <= f64::EPSILON {
                return Ok([0; 4]);
            }
            Ok([
                (color[0] / alpha).round().clamp(0., 65535.) as u16,
                (color[1] / alpha).round().clamp(0., 65535.) as u16,
                (color[2] / alpha).round().clamp(0., 65535.) as u16,
                alpha.round().clamp(0., 65535.) as u16,
            ])
        }
    }
}

fn regions(bounds: [i32; 4], limit: usize) -> Result<Vec<Region>, String> {
    let [l, t, r, b] = bounds;
    if l >= r || t >= b {
        return Ok(Vec::new());
    }
    let width = (r - l) as usize;
    if width
        .checked_mul((b - t) as usize)
        .is_none_or(|n| n > limit)
        || width > MAX_REGION_PIXELS
    {
        return Err("Liquify mapped footprint exceeds its explicit prepared-pixel limit".into());
    }
    let mut regions = Vec::new();
    let rows = (MAX_REGION_PIXELS / width).max(1) as i32;
    let count = ((b - t) as usize).div_ceil(rows as usize);
    regions
        .try_reserve_exact(count)
        .map_err(|_| "Liquify region allocation failed")?;
    let mut y = t;
    while y < b {
        let next = (y + rows).min(b);
        regions.push(Region::new([l, y, r, next]).map_err(|e| e.to_string())?);
        y = next;
    }
    Ok(regions)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{depth16::Image16, effects::Image, large_document::PinnedBytes};
    use serde_json::json;
    fn pixel(x: i32, y: i32, depth: u16) -> Pixel16 {
        let value = [
            ((x * 739 + y * 811 + 12347) % 65536) as u16,
            ((x * 509 + y * 317 + 33559) % 65536) as u16,
            51237,
            if (x + y) % 9 == 0 {
                0
            } else {
                ((x * 137 + y * 797 + 719) % 65536) as u16
            },
        ];
        if depth == 8 {
            value.map(|v| u16::from(project16(v)) * 257)
        } else {
            value
        }
    }
    #[test]
    fn regional_inverse_mapping_and_native_samples_match_existing_full_frame() {
        let (w, h) = (61, 67);
        let settings = json!({"radius":14.25,"strength":0.85,"strokes":[
            {"mode":"push","points":[[-3.5,16.75],[25.25,35.5],[63.25,49.75]]},
            {"mode":"expand","radius":18.75,"points":[[31.25,24.75]]},
            {"mode":"pinch","radius":11.5,"points":[[45.5,44.25]]},
            {"mode":"restore","radius":8.5,"points":[[27.5,33.75]]}
        ]});
        let pool = ResidentBudget::new(8 * 1024 * 1024).unwrap();
        let mapping = Plan::new(
            Frame::new(w, h).unwrap(),
            &settings,
            &[None; 4],
            &pool,
            &mut || Ok(()),
        )
        .unwrap()
        .prepare(&mut || Ok(()), |_, _| panic!("No clips"))
        .unwrap();
        let old_mapping = crate::liquify::mapping16(w, h, &settings).unwrap();
        for y in 0..h as i32 {
            for x in 0..w as i32 {
                assert_eq!(
                    mapping
                        .source_position(x, y)
                        .unwrap()
                        .map(|p| p.map(f32::to_bits)),
                    old_mapping
                        .source_position(x, y)
                        .map(|p| p.map(f32::to_bits))
                );
            }
        }
        let output = Region::new([0, 0, w as i32, h as i32]).unwrap();
        let prepared = mapping.source_regions(output, &mut || Ok(())).unwrap();
        for depth in [8, 16] {
            let mut eight = Image {
                width: w,
                height: h,
                bytes: Vec::new(),
            };
            let mut sixteen = Image16 {
                width: w,
                height: h,
                words: Vec::new(),
            };
            for y in 0..h as i32 {
                for x in 0..w as i32 {
                    let p = pixel(x, y, depth);
                    eight.bytes.extend(p.map(project16));
                    sixteen.words.extend(p);
                }
            }
            if depth == 8 {
                crate::liquify::apply(&mut eight, &settings).unwrap();
            } else {
                crate::depth16::color::apply_cpu(&mut sixteen, "liquify", &settings).unwrap();
            }
            for y in 0..h as i32 {
                for x in 0..w as i32 {
                    let actual = mapping
                        .sample(x, y, depth, |sx, sy| {
                            assert!(prepared.iter().any(|r| {
                                let b = r.bounds();
                                sx >= b[0] && sy >= b[1] && sx < b[2] && sy < b[3]
                            }));
                            Ok(pixel(sx, sy, depth))
                        })
                        .unwrap();
                    let expected = if depth == 8 {
                        eight.get(x, y).map(|v| u16::from(v) * 257)
                    } else {
                        sixteen.get(x, y)
                    };
                    assert_eq!(actual, expected, "depth={depth} ({x},{y})");
                }
            }
        }
        assert!(mapping
            .sample(31, 24, 16, |_, _| Err("Native source missing".into()))
            .is_err());
        assert!(pool.used() > 0);
        drop(mapping);
        assert_eq!(pool.used(), 0);
    }

    struct Clip {
        coverage: crate::selection::Coverage,
        regions: Vec<Region>,
        proof: PinnedBytes,
        _charge: Reservation,
    }
    impl PreparedClip for Clip {
        fn uses_budget(&self, pool: &ResidentBudget) -> bool {
            self.proof.uses_budget(pool)
        }
        fn value(&self, x: i32, y: i32) -> Result<f32, String> {
            let c = &self.coverage;
            let sx = x - c.origin[0];
            let sy = y - c.origin[1];
            if sx < 0 || sy < 0 || sx >= c.mask.width as i32 || sy >= c.mask.height as i32 {
                return Ok(0.);
            }
            if !self.regions.iter().any(|r| {
                let b = r.bounds();
                x >= b[0] && y >= b[1] && x < b[2] && y < b[3]
            }) {
                return Err("Clip sample outside the prepared physical footprint".into());
            }
            Ok(c.value(x, y))
        }
    }
    #[test]
    fn typed_historic_soft_clip_preserves_continuous_geometry_and_legacy_falloff() {
        use crate::large_document::ScratchBytes;
        let pool = ResidentBudget::new(8 * 1024 * 1024).unwrap();
        let (w, h) = (61, 67);
        let mut mask = crate::raster::Raster::new(w, h);
        for y in 0..h as i32 {
            for x in 0..w as i32 {
                let v = ((x * 37 + y * 51) % 256) as u8;
                mask.set(x, y, [v, 0, 0, 17]);
            }
        }
        let coverage = crate::selection::Coverage {
            mask,
            bounds: [4, 5, 9, 11],
            origin: [-2, 1],
            contours: vec![],
        };
        let settings = json!({"radius":17.25,"strokes":[{"points":[[5.25,8.75],[35.25,38.75],[57.5,59.5]],
            "selection":[3,4,56,61],"polygon":[[3.,4.],[56.,4.],[52.5,61.],[9.25,61.]],
            "coverage":coverage}]});
        // Polygon extrema exactly match the retained continuous rectangle.
        let old = crate::liquify::mapping16(w, h, &settings).unwrap();
        let mut pure = settings.clone();
        pure["strokes"][0]["coverage"] = Value::Null;
        assert!(Plan::new(
            Frame::new(w, h).unwrap(),
            &settings,
            &[Some([-2, 1, 59, 68])],
            &pool,
            &mut || Ok(())
        )
        .is_err());
        let mapping = Plan::new(
            Frame::new(w, h).unwrap(),
            &pure,
            &[Some([-2, 1, 59, 68])],
            &pool,
            &mut || Ok(()),
        )
        .unwrap()
        .prepare(&mut || Ok(()), |_, regions| {
            Ok(Box::new(Clip {
                coverage: coverage.clone(),
                regions: regions.to_vec(),
                proof: ScratchBytes::new(&pool, 0, 0).unwrap().freeze(),
                _charge: pool
                    .reserve_bytes(coverage.mask.bytes(), coverage.mask.bytes())
                    .unwrap(),
            }))
        })
        .unwrap();
        for y in 0..h as i32 {
            for x in 0..w as i32 {
                assert_eq!(
                    mapping
                        .source_position(x, y)
                        .unwrap()
                        .map(|p| p.map(f32::to_bits)),
                    old.source_position(x, y).map(|p| p.map(f32::to_bits)),
                    "({x},{y})"
                );
            }
        }
        drop(mapping);
        assert_eq!(pool.used(), 0);
    }

    #[test]
    fn region_mapping_admission_and_cancellation_never_publish_partial_fields() {
        let settings =
            json!({"radius":12,"strokes":[{"points":[[15800.25,15810.5],[15825.5,15835.75]]}]});
        let frame = Frame::new(16000, 16000).unwrap();
        let tiny = ResidentBudget::new(1024).unwrap();
        assert!(Plan::new(frame, &settings, &[None], &tiny, &mut || Ok(())).is_err());
        assert_eq!(tiny.used(), 0);
        let pool = ResidentBudget::new(8 * 1024 * 1024).unwrap();
        let plan = Plan::new(frame, &settings, &[None], &pool, &mut || Ok(())).unwrap();
        let error = plan
            .prepare(
                &mut || Err("Operation canceled or stale".into()),
                |_, _| panic!(),
            )
            .err()
            .unwrap();
        assert_eq!(error, "Operation canceled or stale");
        assert_eq!(pool.used(), 0);
        let mapping = Plan::new(frame, &settings, &[None], &pool, &mut || Ok(()))
            .unwrap()
            .prepare(&mut || Ok(()), |_, _| panic!())
            .unwrap();
        let output = Region::new([15800, 15800, 15840, 15850]).unwrap();
        let regions = mapping.source_regions(output, &mut || Ok(())).unwrap();
        assert!(regions.iter().all(|r| r.pixels() <= MAX_REGION_PIXELS));
        assert!(regions.iter().any(|r| r.bounds()[0] > 15000));
        drop(mapping);
        assert_eq!(pool.used(), 0);
        // This verifies a local mapping at large coordinates, not full editor qualification.
    }

    #[test]
    fn distinct_historic_clip_owners_are_not_deduplicated_by_equal_geometry() {
        use crate::{large_document::ScratchBytes, raster::Raster, selection::Coverage};
        let (w, h) = (61, 67);
        let empty = Coverage {
            mask: Raster::new(w, h),
            bounds: [0, 0, w as i32, h as i32],
            origin: [0, 0],
            contours: vec![],
        };
        let mut full = empty.clone();
        for y in 0..h as i32 {
            for x in 0..w as i32 {
                full.mask.set(x, y, [255, 0, 0, 255]);
            }
        }
        let pure = serde_json::json!({"radius":9.25,"strokes":[
            {"points":[[8.25,9.5],[11.25,12.75]]},
            {"points":[[23.25,24.5],[29.25,31.75]]}
        ]});
        let mut legacy = pure.clone();
        legacy["strokes"][0]["coverage"] = serde_json::to_value(&empty).unwrap();
        legacy["strokes"][1]["coverage"] = serde_json::to_value(&full).unwrap();
        let broken = crate::liquify::mapping16(w, h, &legacy).unwrap();
        let mut second_only = legacy.clone();
        second_only["strokes"] = serde_json::json!([legacy["strokes"][1]]);
        let expected = crate::liquify::mapping16(w, h, &second_only).unwrap();
        let pool = ResidentBudget::new(8 * 1024 * 1024).unwrap();
        let actual = Plan::new(
            Frame::new(w, h).unwrap(),
            &pure,
            &[Some([0, 0, 61, 67]); 2],
            &pool,
            &mut || Ok(()),
        )
        .unwrap()
        .prepare(&mut || Ok(()), |i, regions| {
            let coverage = if i == 0 { empty.clone() } else { full.clone() };
            let bytes = coverage.mask.bytes();
            Ok(Box::new(Clip {
                coverage,
                regions: regions.to_vec(),
                proof: ScratchBytes::new(&pool, 0, 0).unwrap().freeze(),
                _charge: pool.reserve_bytes(bytes, bytes).unwrap(),
            }))
        })
        .unwrap();
        let mut regression = 0;
        for y in 0..h as i32 {
            for x in 0..w as i32 {
                let wanted = expected.source_position(x, y).map(|p| p.map(f32::to_bits));
                assert_eq!(
                    actual
                        .source_position(x, y)
                        .unwrap()
                        .map(|p| p.map(f32::to_bits)),
                    wanted
                );
                if wanted.is_some() && broken.source_position(x, y).is_none() {
                    regression += 1;
                }
            }
        }
        assert!(
            regression > 0,
            "Fixture must reproduce the old geometry-only clip deduplication fault"
        );
        drop(actual);
        assert_eq!(pool.used(), 0);
    }

    #[test]
    fn native16_bilinear_preserves_hidden_rgb_below_the_native8_alpha_cutoff() {
        let settings = json!({"radius":12.25,"strength":0.0001,
            "strokes":[{"points":[[10.25,10.5],[20.75,21.25]]}]});
        let pool = ResidentBudget::new(4 * 1024 * 1024).unwrap();
        let mapping = Plan::new(
            Frame::new(32, 32).unwrap(),
            &settings,
            &[None],
            &pool,
            &mut || Ok(()),
        )
        .unwrap()
        .prepare(&mut || Ok(()), |_, _| panic!())
        .unwrap();
        let (x, y, position, corner) = (0..32)
            .flat_map(|y| (0..32).map(move |x| (x, y)))
            .find_map(|(x, y)| {
                let p = mapping.source_position(x, y).unwrap()?;
                let ix = p[0].floor() as i32;
                let iy = p[1].floor() as i32;
                let fx = f64::from(p[0]) - f64::from(ix);
                let fy = f64::from(p[1]) - f64::from(iy);
                [
                    (ix, iy, (1. - fx) * (1. - fy)),
                    (ix + 1, iy, fx * (1. - fy)),
                    (ix, iy + 1, (1. - fx) * fy),
                    (ix + 1, iy + 1, fx * fy),
                ]
                .into_iter()
                .find(|(sx, sy, w)| {
                    *sx >= 0
                        && *sy >= 0
                        && *sx < 32
                        && *sy < 32
                        && *w > f64::EPSILON
                        && *w < 0.00001
                })
                .map(|(sx, sy, _)| (x, y, p, [sx, sy]))
            })
            .expect("Fixture must exercise a positive native16 alpha below the old 8-bit cutoff");
        let mut image = Image16 {
            width: 32,
            height: 32,
            words: vec![0; 32 * 32 * 4],
        };
        let at = (corner[1] as usize * 32 + corner[0] as usize) * 4;
        image.words[at..at + 4].copy_from_slice(&[12347, 33559, 51237, 1]);
        let actual = mapping
            .sample(x, y, 16, |sx, sy| Ok(image.get(sx, sy)))
            .unwrap();
        let expected =
            crate::depth16::color::sample(&image, f64::from(position[0]), f64::from(position[1]));
        assert_eq!(actual, expected);
        assert_eq!(actual, [12347, 33559, 51237, 0]);
        drop(mapping);
        assert_eq!(pool.used(), 0);
    }
}
