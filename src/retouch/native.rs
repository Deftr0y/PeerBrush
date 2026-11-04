//! Explicit clone/heal sampling footprints and native memory-only tile kernels.
//! Preparation and document publication remain the source worker's responsibility.
use crate::{
    brush::{native::PreparedBrush, Settings},
    large_document::{PinnedBytes, ResidentBudget, ScratchBytes},
    raster::{blend16, project16, Pixel16, TILE},
};

pub(crate) struct ReadFootprints {
    /// Integer target-local rectangle, including healing halo, half-open.
    pub destination: Option<[i32; 4]>,
    /// Inclusive min/max floating source sampling queries in document space.
    /// The source adapter must add exact bilinear support after its own origin.
    pub source_queries: [f32; 4],
}

pub(crate) struct PreparedRetouch<'a> {
    coverage: PreparedBrush<'a>,
    width: u32,
    height: u32,
    depth: u16,
    origin: [i32; 2],
    offset: [f32; 2],
    opacity: f32,
    heal_radius: Option<i32>,
    budget: ResidentBudget,
}

impl<'a> PreparedRetouch<'a> {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        width: u32,
        height: u32,
        depth: u16,
        points_local: &[[f32; 2]],
        pressures: Option<&[f32]>,
        mut settings: Settings,
        origin: [i32; 2],
        offset_document: [f32; 2],
        heal_radius: Option<u32>,
        clip: Option<[i32; 4]>,
        polygon_local: Option<&'a [[f32; 2]]>,
        budget: &ResidentBudget,
        check: &mut dyn FnMut() -> Result<(), String>,
    ) -> Result<Self, String> {
        if origin.iter().any(|v| v.unsigned_abs() > 100_000)
            || offset_document
                .iter()
                .any(|v| !v.is_finite() || v.abs() > 400_000.)
            || heal_radius.is_some_and(|r| !(1..=64).contains(&r))
        {
            return Err("Invalid native clone/heal geometry".into());
        }
        settings.texture_origin = origin;
        let coverage = PreparedBrush::new(
            width,
            height,
            depth,
            points_local,
            pressures,
            settings,
            [255; 4],
            false,
            clip,
            polygon_local,
            budget,
            check,
        )?;
        Ok(Self {
            coverage,
            width,
            height,
            depth,
            origin,
            offset: offset_document,
            opacity: settings.opacity,
            heal_radius: heal_radius.map(|r| r as i32),
            budget: budget.clone(),
        })
    }

    pub(crate) fn tiles(&self) -> &[(u32, u32)] {
        self.coverage.tiles()
    }

    fn tile_bounds(&self, key: (u32, u32)) -> Result<[i32; 4], String> {
        if self
            .tiles()
            .binary_search_by_key(&(key.1, key.0), |&(x, y)| (y, x))
            .is_err()
        {
            return Err("Native retouch tile is outside its prepared footprint".into());
        }
        Ok([
            (key.0 * TILE) as i32,
            (key.1 * TILE) as i32,
            ((key.0 + 1) * TILE).min(self.width) as i32,
            ((key.1 + 1) * TILE).min(self.height) as i32,
        ])
    }

    fn source_x(&self, x: i32) -> f32 {
        x as f32 + self.origin[0] as f32 + self.offset[0]
    }
    fn source_y(&self, y: i32) -> f32 {
        y as f32 + self.origin[1] as f32 + self.offset[1]
    }

    pub(crate) fn tile_reads(&self, key: (u32, u32)) -> Result<ReadFootprints, String> {
        let bounds = self.tile_bounds(key)?;
        let radius = self.heal_radius.unwrap_or(0);
        let area = [
            bounds[0] - radius,
            bounds[1] - radius,
            bounds[2] + radius,
            bounds[3] + radius,
        ];
        Ok(ReadFootprints {
            destination: self.heal_radius.map(|_| area),
            source_queries: [
                self.source_x(area[0]),
                self.source_y(area[1]),
                self.source_x(area[2] - 1),
                self.source_y(area[3] - 1),
            ],
        })
    }

    /// All callbacks sample already-prepared memory. Missing footprints and every
    /// source/cancellation error propagate before an immutable result is returned.
    pub(crate) fn render_tile(
        &self,
        key: (u32, u32),
        before: &PinnedBytes,
        check: &mut dyn FnMut() -> Result<(), String>,
        mut destination: impl FnMut(i32, i32) -> Result<Pixel16, String>,
        mut source: impl FnMut(f32, f32) -> Result<Pixel16, String>,
    ) -> Result<PinnedBytes, String> {
        check()?;
        let bounds = self.tile_bounds(key)?;
        let length = (TILE * TILE) as usize * if self.depth == 16 { 8 } else { 4 };
        if !before.uses_budget(&self.budget) || before.len() != length {
            return Err("Native retouch input ownership or length differs".into());
        }
        let coverage = self.coverage.coverage16_tile(key, check)?;
        let mut output =
            ScratchBytes::new(&self.budget, length, length).map_err(|e| e.to_string())?;
        output.copy_from_slice(before);
        let means = if let Some(radius) = self.heal_radius {
            let area = [
                bounds[0] - radius,
                bounds[1] - radius,
                bounds[2] + radius,
                bounds[3] + radius,
            ];
            Some((
                Integral::new(area, &self.budget, check, &mut destination)?,
                Integral::new(area, &self.budget, check, |x, y| {
                    source(self.source_x(x), self.source_y(y))
                })?,
            ))
        } else {
            None
        };
        for y in bounds[1]..bounds[3] {
            check()?;
            for x in bounds[0]..bounds[2] {
                let pixel = ((y - bounds[1]) * TILE as i32 + x - bounds[0]) as usize;
                let alpha = u16::from_be_bytes([coverage[pixel * 2], coverage[pixel * 2 + 1]]);
                let amount = alpha as f64 / 65535. * self.opacity as f64;
                if amount <= 0. {
                    continue;
                }
                let at = pixel * if self.depth == 16 { 8 } else { 4 };
                let old = if self.depth == 16 {
                    std::array::from_fn(|i| {
                        u16::from_be_bytes([before[at + i * 2], before[at + i * 2 + 1]])
                    })
                } else {
                    std::array::from_fn(|i| u16::from(before[at + i]) * 257)
                };
                let mut sampled = source(self.source_x(x), self.source_y(y))?;
                if sampled[3] == 0 {
                    continue;
                }
                let result = if let Some((destination, source)) = &means {
                    if old[3] == 0 {
                        continue;
                    }
                    let radius = self.heal_radius.unwrap();
                    let (Some(destination), Some(source)) =
                        (destination.at(x, y, radius)?, source.at(x, y, radius)?)
                    else {
                        continue;
                    };
                    let amount = amount * sampled[3] as f64 / 65535.;
                    let mut result = old;
                    for c in 0..3 {
                        let corrected =
                            (sampled[c] as f64 + destination[c] - source[c]).clamp(0., 65535.);
                        result[c] =
                            (old[c] as f64 + (corrected - old[c] as f64) * amount).round() as u16;
                    }
                    result
                } else {
                    sampled[3] = (sampled[3] as f64 * amount).round() as u16;
                    blend16(old, sampled, 1., "normal")
                };
                if self.depth == 16 {
                    for (dst, value) in output[at..at + 8].chunks_exact_mut(2).zip(result) {
                        dst.copy_from_slice(&value.to_be_bytes());
                    }
                } else {
                    output[at..at + 4].copy_from_slice(&result.map(project16));
                }
            }
        }
        check()?;
        Ok(output.freeze())
    }
}

/// Exactly the legacy per-tile integral order, using charged canonical f64 bytes
/// so no uncharged or alignment-dependent native array is hidden in the kernel.
struct Integral {
    bytes: ScratchBytes,
    stride: usize,
    rows: usize,
    left: i32,
    top: i32,
}
impl Integral {
    fn new(
        area: [i32; 4],
        budget: &ResidentBudget,
        check: &mut dyn FnMut() -> Result<(), String>,
        mut sample: impl FnMut(i32, i32) -> Result<Pixel16, String>,
    ) -> Result<Self, String> {
        let width = (area[2] - area[0]) as usize;
        let height = (area[3] - area[1]) as usize;
        if width == 0 || height == 0 || width > 384 || height > 384 {
            return Err("Invalid native healing halo".into());
        }
        let stride = width + 1;
        let rows = height + 1;
        let length = stride * rows * 32;
        let bytes = ScratchBytes::new(budget, length, length).map_err(|e| e.to_string())?;
        let mut out = Self {
            bytes,
            stride,
            rows,
            left: area[0],
            top: area[1],
        };
        for y in 0..height {
            check()?;
            let mut row = [0.; 4];
            for x in 0..width {
                let pixel = sample(x as i32 + area[0], y as i32 + area[1])?;
                let alpha = pixel[3] as f64 / 65535.;
                let above = out.cell(x + 1, y);
                for c in 0..4 {
                    row[c] += if c == 3 {
                        alpha
                    } else {
                        pixel[c] as f64 * alpha
                    };
                    let sum = row[c] + above[c];
                    let at = ((y + 1) * stride + x + 1) * 32 + c * 8;
                    out.bytes[at..at + 8].copy_from_slice(&sum.to_be_bytes());
                }
            }
        }
        check()?;
        Ok(out)
    }
    fn cell(&self, x: usize, y: usize) -> [f64; 4] {
        let at = (y * self.stride + x) * 32;
        std::array::from_fn(|c| {
            f64::from_be_bytes(self.bytes[at + c * 8..at + c * 8 + 8].try_into().unwrap())
        })
    }
    fn at(&self, x: i32, y: i32, radius: i32) -> Result<Option<[f64; 3]>, String> {
        let [x0, y0, x1, y1] = [
            x - radius - self.left,
            y - radius - self.top,
            x + radius + 1 - self.left,
            y + radius + 1 - self.top,
        ];
        if x0 < 0 || y0 < 0 || x1 as usize >= self.stride || y1 as usize >= self.rows {
            return Err("Healing sample is outside its prepared integral field".into());
        }
        let a = self.cell(x1 as usize, y1 as usize);
        let b = self.cell(x1 as usize, y0 as usize);
        let c = self.cell(x0 as usize, y1 as usize);
        let d = self.cell(x0 as usize, y0 as usize);
        let sum: [f64; 4] = std::array::from_fn(|i| a[i] - b[i] - c[i] + d[i]);
        Ok((sum[3] > 0.).then(|| [sum[0] / sum[3], sum[1] / sum[3], sum[2] / sum[3]]))
    }
}

/// Exact Raster::sample16 arithmetic over explicit fallible native pixels.
pub(crate) fn bilinear(
    x: f32,
    y: f32,
    mut pixel: impl FnMut(i32, i32) -> Result<Pixel16, String>,
) -> Result<Pixel16, String> {
    if !x.is_finite() || !y.is_finite() || x.abs() > 400_000. || y.abs() > 400_000. {
        return Err("Invalid native retouch sample coordinate".into());
    }
    if x.fract() == 0. && y.fract() == 0. {
        return pixel(x as i32, y as i32);
    }
    let ix = x.floor() as i32;
    let iy = y.floor() as i32;
    let fx = (x - x.floor()) as f64;
    let fy = (y - y.floor()) as f64;
    let mut sum = [0.; 4];
    for (dx, dy, weight) in [
        (0, 0, (1. - fx) * (1. - fy)),
        (1, 0, fx * (1. - fy)),
        (0, 1, (1. - fx) * fy),
        (1, 1, fx * fy),
    ] {
        let value = pixel(ix + dx, iy + dy)?;
        let alpha = value[3] as f64 / 65535.;
        for c in 0..3 {
            sum[c] += value[c] as f64 * alpha * weight;
        }
        sum[3] += alpha * weight;
    }
    if sum[3] <= 0. {
        return Ok([0; 4]);
    }
    Ok([
        (sum[0] / sum[3]).round().clamp(0., 65535.) as u16,
        (sum[1] / sum[3]).round().clamp(0., 65535.) as u16,
        (sum[2] / sum[3]).round().clamp(0., 65535.) as u16,
        (sum[3] * 65535.).round().clamp(0., 65535.) as u16,
    ])
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn clone_heal_tiles_match_resident_operator_at_both_depths() {
        for depth in [8, 16] {
            for heal in [false, true] {
                let mut doc = crate::engine::Document::new_depth(273, 265, depth).unwrap();
                for y in 0..265 {
                    for x in 0..273 {
                        doc.layers[0].pixels.set16(
                            x,
                            y,
                            [
                                12347u16.wrapping_add(x as u16 * 17),
                                33559u16.wrapping_sub(y as u16 * 31),
                                51237u16.wrapping_add((x + y) as u16),
                                if (x + y) % 13 == 0 { 0 } else { 45679 },
                            ],
                        );
                    }
                }
                let original = doc.layers[0].pixels.clone();
                let command = serde_json::json!({
                    "op": if heal { "heal" } else { "clone" },
                    "points": [[14.25,17.5,0.27],[131.75,129.25,0.91],[267.25,261.75,0.63]],
                    "source": [23.5,10.], "radius":9.3, "hardness":0.37,
                    "opacity":0.71, "flow":0.38, "spacing":0.21, "tip":"grain", "heal_radius":9
                });
                crate::retouch::apply(&mut doc, 0, &command).unwrap();
                let (points, pressures) = crate::brush::points_from_command(&command).unwrap();
                let settings = Settings::from_command(&command).unwrap();
                let pool = ResidentBudget::new(64 * 1024 * 1024).unwrap();
                let plan = PreparedRetouch::new(
                    273,
                    265,
                    depth,
                    &points,
                    pressures.as_deref(),
                    settings,
                    [0, 0],
                    [23.5 - points[0][0], 10. - points[0][1]],
                    heal.then_some(9),
                    None,
                    None,
                    &pool,
                    &mut || Ok(()),
                )
                .unwrap();
                for &key in plan.tiles() {
                    let length = (TILE * TILE) as usize * if depth == 16 { 8 } else { 4 };
                    let copy = |r: &crate::raster::Raster| {
                        let mut bytes = ScratchBytes::new(&pool, length, length).unwrap();
                        if depth == 16 {
                            if let Some(tile) = r.samples16.get(&key) {
                                for (dst, value) in bytes.chunks_exact_mut(2).zip(tile.iter()) {
                                    dst.copy_from_slice(&value.to_be_bytes());
                                }
                            }
                        } else if let Some(tile) = r.tiles.get(&key) {
                            bytes.copy_from_slice(tile);
                        }
                        bytes.freeze()
                    };
                    let before = copy(&original);
                    let output = plan
                        .render_tile(
                            key,
                            &before,
                            &mut || Ok(()),
                            |x, y| Ok(original.get16(x, y)),
                            |x, y| bilinear(x, y, |x, y| Ok(original.get16(x, y))),
                        )
                        .unwrap();
                    assert_eq!(
                        &*output,
                        &*copy(&doc.layers[0].pixels),
                        "native retouch depth{depth} heal{heal} key{key:?}"
                    );
                }
                drop(plan);
                assert_eq!(pool.used(), 0);
            }
        }
    }
    #[test]
    fn explicit_bilinear_matches_native_sampling_and_never_hides_missing_pixels() {
        let mut raster = crate::raster::Raster::new_depth(5, 4, 16);
        for y in 0..4 {
            for x in 0..5 {
                raster.set16(
                    x,
                    y,
                    [
                        12347 + x as u16 * 173,
                        33559,
                        51237,
                        if (x + y) % 3 == 0 { 0 } else { 45679 },
                    ],
                );
            }
        }
        for y in [-0.75, 0., 0.25, 1., 2.5, 3.75, 4.] {
            for x in [-0.75, 0., 0.25, 1., 3.5, 4.75, 5.] {
                assert_eq!(
                    bilinear(x, y, |x, y| Ok(raster.get16(x, y))).unwrap(),
                    raster.sample16(x, y)
                );
            }
        }
        assert!(bilinear(1., 0.5, |x, _| if x == 2 {
            Err("Missing zero-weight footprint".into())
        } else {
            Ok([65535; 4])
        })
        .is_err());
    }
    #[test]
    fn halo_fields_are_charged_and_source_errors_cannot_finish_a_tile() {
        let pool = ResidentBudget::new(32 * 1024 * 1024).unwrap();
        let plan = PreparedRetouch::new(
            256,
            256,
            16,
            &[[12., 12.], [90., 90.]],
            None,
            Settings::default(),
            [0, 0],
            [9.25, -7.5],
            Some(64),
            None,
            None,
            &pool,
            &mut || Ok(()),
        )
        .unwrap();
        let reads = plan.tile_reads((0, 0)).unwrap();
        assert_eq!(reads.destination, Some([-64, -64, 320, 320]));
        assert_eq!(reads.source_queries, [-54.75, -71.5, 328.25, 311.5]);
        let before = ScratchBytes::new(&pool, 512 * 1024, 512 * 1024)
            .unwrap()
            .freeze();
        let charged = pool.used();
        let mut samples = 0;
        assert!(plan
            .render_tile(
                (0, 0),
                &before,
                &mut || Ok(()),
                |_, _| {
                    samples += 1;
                    if samples == 3 {
                        Err("Missing destination halo".into())
                    } else {
                        Ok([65535; 4])
                    }
                },
                |_, _| Ok([65535; 4])
            )
            .is_err());
        assert_eq!(pool.used(), charged);
        assert!(before.iter().all(|v| *v == 0));
        let mut checks = 0;
        assert!(plan
            .render_tile(
                (0, 0),
                &before,
                &mut || {
                    checks += 1;
                    if checks > 3 {
                        Err("Cancelled retouch".into())
                    } else {
                        Ok(())
                    }
                },
                |_, _| Ok([65535; 4]),
                |_, _| Ok([65535; 4])
            )
            .is_err());
        assert_eq!(pool.used(), charged);
        drop(before);
        drop(plan);
        assert_eq!(pool.used(), 0);
    }
}
