//! Bounded image sampling over the shared renderer's captured display pixels.
use super::{
    backed::CoverageSource,
    source::PriorRows,
    worker::{self, Combine},
    SelectionSource,
};
use crate::{
    large_document::{Error, ScratchBytes, Store},
    raster::{
        backed::{Region, MAX_REGION_PIXELS},
        surface::Surface,
    },
};
use serde_json::Value;
use std::cell::RefCell;

pub(super) fn difference(a: &[u8], b: &[u8], color_similarity: bool) -> f32 {
    if color_similarity {
        return (0..4)
            .map(|i| (a[i] as f32 - b[i] as f32).abs())
            .fold(0., f32::max);
    }
    let rgb = (0..3)
        .map(|i| (a[i] as f32 - b[i] as f32).powi(2))
        .sum::<f32>()
        .sqrt()
        / 3.0_f32.sqrt();
    rgb.max((a[3] as f32 - b[3] as f32).abs())
}

pub(crate) struct Plan<'a> {
    size: [u32; 2],
    bounds: [i32; 4],
    point: [i32; 2],
    points: Option<&'a [Value]>,
    object: bool,
    contiguous: bool,
    quick_radius: Option<u32>,
    tolerance: f32,
    similarity: bool,
    feather: u32,
    mode: Combine,
}

impl<'a> Plan<'a> {
    pub(crate) fn capture(c: &'a Value, w: u32, h: u32) -> Result<Self, String> {
        crate::raster::CanvasLimits::Native.check(w, h)?;
        let number = |key, default| c[key].as_f64().filter(|v| v.is_finite()).unwrap_or(default);
        let similarity = c
            .get("color_similarity")
            .map(|v| v.as_bool().ok_or("Color similarity must be boolean"))
            .transpose()?
            .unwrap_or(false);
        if similarity {
            if let Some(v) = c.get("tolerance") {
                v.as_f64()
                    .filter(|v| v.is_finite() && *v >= 0. && *v <= 255.)
                    .ok_or("Similarity tolerance must be between 0 and 255")?;
            }
        }
        let tolerance = number("tolerance", 24.).clamp(0., 255.) as f32;
        let feather = number("feather", 0.).round().clamp(0., 64.) as u32;
        let mode = match c["mode"].as_str().unwrap_or("replace") {
            "replace" => Combine::Replace,
            "add" => Combine::Add,
            "subtract" => Combine::Subtract,
            "intersect" => Combine::Intersect,
            _ => return Err("Choose replace, add, subtract or intersect".into()),
        };
        let area = c
            .get("rect")
            .filter(|r| !r.is_null())
            .map(|r| crate::engine::rect(r).ok_or("Invalid rectangle coordinates"))
            .transpose()?
            .unwrap_or([0, 0, w as i32, h as i32]);
        let at = c["point"]
            .as_array()
            .filter(|p| p.len() == 2)
            .map(|p| {
                [
                    p[0].as_f64().unwrap_or(0.).floor() as i32,
                    p[1].as_f64().unwrap_or(0.).floor() as i32,
                ]
            })
            .unwrap_or_else(|| {
                std::array::from_fn(|axis| {
                    ((i64::from(area[axis]) + i64::from(area[axis + 2])) / 2) as i32
                })
            });
        if at[0] < 0 || at[1] < 0 || at[0] >= w as i32 || at[1] >= h as i32 {
            return Err("Select inside the canvas".into());
        }
        let kind = c["kind"].as_str().ok_or("Sampled selection kind missing")?;
        if !["wand", "quick", "object"].contains(&kind) {
            return Err("Unknown sampled selection kind".into());
        }
        let object = kind == "object";
        let contiguous = !object && c["contiguous"] != false;
        let points = if contiguous {
            // Match the resident fallback for malformed point lists without allocating
            // an unbounded intermediate JSON conversion before seed admission.
            c["points"]
                .as_array()
                .filter(|points| {
                    points.iter().all(|p| {
                        p.as_array()
                            .is_some_and(|p| p.len() == 2 && p.iter().all(|v| v.as_f64().is_some()))
                    })
                })
                .map(Vec::as_slice)
        } else {
            None
        };
        if points.is_some_and(|points| points.len() > super::MAX_BRUSH_SEEDS) {
            return Err("Use at most 8192 selection brush seeds".into());
        }
        let quick_radius =
            (contiguous && kind == "quick").then(|| number("radius", 12.).clamp(1., 256.) as u32);
        let mut plan = Self {
            size: [w, h],
            bounds: [0, 0, w as i32, h as i32],
            point: at,
            points,
            object,
            contiguous,
            quick_radius,
            tolerance,
            similarity,
            feather,
            mode,
        };
        if object {
            plan.bounds = [
                area[0].max(0),
                area[1].max(0),
                area[2].min(w as i32),
                area[3].min(h as i32),
            ];
            if plan.bounds[0] >= plan.bounds[2] || plan.bounds[1] >= plan.bounds[3] {
                return Err("Object region must intersect the canvas with positive area".into());
            }
        } else if let Some(radius) = quick_radius {
            let r = radius as i32;
            plan.bounds = (0..plan.seed_count())
                .filter_map(|i| {
                    let [x, y] = plan.seed(i);
                    plan.valid([x, y]).then(|| {
                        [
                            (x - r).max(0),
                            (y - r).max(0),
                            (x + r + 1).min(w as i32),
                            (y + r + 1).min(h as i32),
                        ]
                    })
                })
                .reduce(crate::engine::reframe_scope::union)
                .unwrap_or([at[0], at[1], at[0] + 1, at[1] + 1]);
        }
        Ok(plan)
    }

    fn seed_count(&self) -> usize {
        self.points.map_or(1, |points| points.len())
    }

    fn seed(&self, i: usize) -> [i32; 2] {
        self.points.map_or(self.point, |points| {
            std::array::from_fn(|axis| (points[i][axis].as_f64().unwrap() as f32).floor() as i32)
        })
    }

    fn valid(&self, [x, y]: [i32; 2]) -> bool {
        x >= 0 && y >= 0 && x < self.size[0] as i32 && y < self.size[1] as i32
    }

    pub(crate) fn guide_rect(&self, profiled: bool) -> [i32; 4] {
        let mut bounds = self.bounds;
        // The display conversion uses the original row width and vector boundaries.
        if profiled {
            bounds[0] = 0;
            bounds[2] = self.size[0] as i32;
        }
        bounds
    }

    pub(crate) fn prepare(
        &self,
        guide: &Surface,
        guide_bounds: [i32; 4],
        prior: Option<&SelectionSource>,
        store: Store,
        check: &dyn Fn() -> crate::large_document::Result<()>,
    ) -> Result<CoverageSource, String> {
        let [w, h] = self.size;
        let [left, top, right, bottom] = self.bounds;
        let [gx, gy, gr, gb] = guide_bounds;
        let pool = store.resident_budget();
        let disk = store
            .disk_budget()
            .ok_or("Selection sampling needs shared disk admission")?;
        if guide.depth() != 8
            || !guide.uses_budget(&pool)
            || !guide.uses_disk_budget(&disk)
            || gx < 0
            || gy < 0
            || gr > w as i32
            || gb > h as i32
            || gx > left
            || gy > top
            || gr < right
            || gb < bottom
            || gr - gx != guide.width() as i32
            || gb - gy != guide.height() as i32
        {
            return Err("Selection sampling differs from its admitted display source".into());
        }
        check().map_err(|e| e.to_string())?;
        let sample = |[x, y]: [i32; 2]| {
            let (x, y) = (x - gx, y - gy);
            guide
                .prepare_region(Region::new([x, y, x + 1, y + 1])?, &mut || check())?
                .pixel(x, y)
        };
        let max_rows = (MAX_REGION_PIXELS / (right - left) as usize).min(crate::raster::TILE as usize);
        // Use a power-of-two divisor of the tile height so each band pins at most one tile row.
        let band_height = 1_i32 << max_rows.ilog2();
        let prepare_band = |y: i32| {
            let start = (y - gy) / band_height * band_height;
            let end = (start + band_height).min(bottom - gy);
            let region = Region::new([left - gx, start.max(top - gy), right - gx, end])?;
            guide.prepare_region(region, &mut || check())
        };
        if self.contiguous {
            let count = self.seed_count();
            let bytes = count * std::mem::size_of::<[u32; 2]>();
            let _charge = pool
                .reserve_bytes(bytes, bytes)
                .map_err(|e| e.to_string())?;
            let mut seeds = Vec::new();
            seeds
                .try_reserve_exact(count)
                .map_err(|_| "Selection seed allocation refused")?;
            if seeds.capacity() * std::mem::size_of::<[u32; 2]>() > bytes {
                return Err("Selection seeds exceed admission".into());
            }
            let mut references =
                ScratchBytes::new(&pool, count * 4, count * 4).map_err(|e| e.to_string())?;
            for i in 0..count {
                check().map_err(|e| e.to_string())?;
                let point = self.seed(i);
                if self.valid(point) {
                    seeds.push([(point[0] - left) as u32, (point[1] - top) as u32]);
                    references[i * 4..i * 4 + 4]
                        .copy_from_slice(&sample(point).map_err(|e| e.to_string())?);
                } else {
                    seeds.push([u32::MAX; 2]);
                }
            }
            let band = RefCell::new(None::<crate::raster::surface::SurfaceRegion>);
            return worker::prepare_in_region(
                &worker::Request {
                    width: w,
                    height: h,
                    seeds: &seeds,
                    quick_radius: self.quick_radius,
                    feather: self.feather,
                    mode: self.mode,
                    maximum_work: 64_000_000.max(u64::from(w) * u64::from(h) * 4),
                },
                self.bounds.map(|v| v as u32),
                store,
                prior,
                check,
                |y| {
                    let y = y as i32;
                    let mut band = band.borrow_mut();
                    if band.as_ref().is_some_and(|view| {
                        let [_, top, _, bottom] = view.region().bounds();
                        (top..bottom).contains(&(y - gy))
                    }) {
                        return Ok(());
                    }
                    drop(band.take());
                    *band = Some(prepare_band(y)?);
                    Ok(())
                },
                |seed, x, y| {
                    let band = band.borrow();
                    let pixel = band
                        .as_ref()
                        .ok_or(Error::Invalid("Selection image band missing"))?
                        .pixel(x as i32 - gx, y as i32 - gy)?;
                    Ok(
                        difference(&pixel, &references[seed * 4..seed * 4 + 4], self.similarity)
                            <= self.tolerance,
                    )
                },
            );
        }
        let references = if self.object {
            let mut colors = [[0; 4]; 4];
            for (color, point) in colors.iter_mut().zip([
                [left, top],
                [right - 1, top],
                [left, bottom - 1],
                [right - 1, bottom - 1],
            ]) {
                *color = sample(point).map_err(|e| e.to_string())?;
            }
            colors
        } else {
            [sample(self.point).map_err(|e| e.to_string())?; 4]
        };
        let mut previous =
            ScratchBytes::new(&pool, w as usize, w as usize).map_err(|e| e.to_string())?;
        let mut prior_rows = PriorRows::new(prior, h);
        let mut band = None;
        let row_bytes = (right - left) as usize * 4;
        let mut pixels = ScratchBytes::new(&pool, row_bytes, row_bytes).map_err(|e| e.to_string())?;
        super::stage_feathered_rows(
            w,
            h,
            self.feather,
            store,
            &pool,
            check,
            None,
            |y, row| {
                let y = y as i32;
                if !(top..bottom).contains(&y) {
                    row.fill(0);
                    return Ok(());
                }
                if y == top || (y - gy) % band_height == 0 {
                    drop(band.take());
                    band = Some(prepare_band(y).map_err(|e| e.to_string())?);
                }
                band.as_ref().ok_or("Selection image band missing")?
                    .copy_row(y - gy, &mut pixels)
                    .map_err(|e| e.to_string())?;
                row[..left as usize].fill(0);
                row[right as usize..].fill(0);
                for (pixel, value) in pixels.chunks_exact(4).zip(&mut row[left as usize..right as usize]) {
                    let matched = if self.object {
                        pixel[3] > 0
                            && references.iter().all(|reference| {
                                difference(pixel, reference, self.similarity) > self.tolerance
                            })
                    } else {
                        difference(pixel, &references[0], self.similarity) <= self.tolerance
                    };
                    *value = if matched { 255 } else { 0 };
                }
                Ok(())
            },
            |y, row| {
                if self.mode == Combine::Replace {
                    return Ok(());
                }
                prior_rows.copy_row(y, &mut previous, &pool, check).map_err(|e| e.to_string())?;
                for (value, &before) in row.iter_mut().zip(previous.iter()) {
                    *value = self.mode.value(before, *value);
                }
                Ok(())
            },
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn guide_bounds_follow_selection_kind_and_keep_profiled_rows_full_width() {
        let quick = json!({"kind":"quick","points":[[21.8,16.9],[-1,19],[29,19]],"radius":2});
        let plan = Plan::capture(&quick, 30, 20).unwrap();
        assert_eq!(plan.guide_rect(false), [19, 14, 30, 20]);
        assert_eq!(plan.guide_rect(true), [0, 14, 30, 20]);

        let object = json!({"kind":"object","rect":[24,12,34,22]});
        assert_eq!(
            Plan::capture(&object, 30, 20).unwrap().guide_rect(false),
            [24, 12, 30, 20]
        );

        for command in [
            json!({"kind":"wand"}),
            json!({"kind":"quick","contiguous":false}),
        ] {
            assert_eq!(
                Plan::capture(&command, 30, 20).unwrap().guide_rect(false),
                [0, 0, 30, 20]
            );
        }
    }
}
