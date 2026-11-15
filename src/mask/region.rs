//! Native mask stages consume explicitly prepared paint footprints. No full
//! canvas mask, implicit source read or disposable-source fallback is involved.
use crate::{
    effects::{self, regional::Frame},
    large_document::{PinnedBytes, Reservation, ResidentBudget, ScratchBytes},
    raster::{
        backed::{Region, MAX_REGION_PIXELS},
        project16, Pixel16, Surface, SurfaceRegion, TILE,
    },
    workspace::source_runtime::ReadChecks,
};
use serde_json::Value;

const MAX_SPATIAL_SCRATCH_BYTES: usize = 64 * 1024 * 1024;

#[derive(Clone)]
pub(crate) struct Step<'a> {
    pub kind: &'a str,
    pub enabled: bool,
    pub weight: f32,
    pub value: f32,
    pub settings: &'a Value,
}
impl Step<'_> {
    fn paints(&self) -> bool {
        self.enabled && self.weight > 0. && self.kind == "paint"
    }
}
impl<'a> From<crate::engine::native::MaskStepMetadata<'a>> for Step<'a> {
    fn from(step: crate::engine::native::MaskStepMetadata<'a>) -> Self {
        Self {
            kind: step.kind(),
            enabled: step.enabled(),
            weight: step.weight(),
            value: step.value(),
            settings: step.settings(),
        }
    }
}
#[cfg(test)]
impl<'a> From<&'a crate::engine::MaskStep> for Step<'a> {
    fn from(step: &'a crate::engine::MaskStep) -> Self {
        Self {
            kind: &step.kind,
            enabled: step.enabled,
            weight: step.weight,
            value: step.value,
            settings: &step.settings,
        }
    }
}
pub(crate) struct Plan<'a> {
    frame: Frame,
    output: Region,
    grid: Option<[&'a [i32]; 2]>,
    depth: u16,
    cached: bool,
    halo: i32,
    steps: &'a [Step<'a>],
}
fn stack_properties<'a>(
    depth: u16,
    steps: impl ExactSizeIterator<Item = Step<'a>>,
) -> Result<(i32, bool), String> {
    if ![8, 16].contains(&depth) || steps.len() > 64 {
        return Err("Invalid native mask depth or stage count".into());
    }
    let mut halo = 0i32;
    let mut cached = false;
    for step in steps {
        if !step.value.is_finite() {
            return Err("Mask value must be finite".into());
        }
        effects::validate_weight(step.weight)?;
        if ![
            "fill", "paint", "invert", "density", "levels", "curves", "adjust", "blur", "gaussian",
        ]
        .contains(&step.kind)
        {
            return Err("Unsupported regional mask stage".into());
        }
        if ["curves", "adjust"].contains(&step.kind) {
            effects::validate(step.kind, step.settings)?;
        }
        if step.kind == "gaussian" {
            effects::validate("blur", step.settings)?;
        }
        if !step.enabled || step.weight == 0. {
            continue;
        }
        cached |= super::needs_cache_step(step.kind, step.enabled, step.weight, step.value);
        let radius = match step.kind {
            "gaussian" => effects::gaussian_radii(effects::number(step.settings, "radius", 8.))
                .iter()
                .sum::<usize>() as i32,
            "blur" if step.value >= 0.5 => {
                3 * (step.value.round().clamp(1., 64.) as usize).div_ceil(3) as i32
            }
            _ => 0,
        };
        halo = halo.checked_add(radius).ok_or("Mask halo overflow")?;
    }
    Ok((halo, cached))
}

pub(crate) fn native_sample_fits<'a>(
    width: u32,
    height: u32,
    depth: u16,
    steps: impl ExactSizeIterator<Item = Step<'a>>,
) -> Result<bool, String> {
    let frame = Frame::new(width, height)?;
    let (halo, spatial) = stack_properties(depth, steps)?;
    if !spatial {
        return Ok(true);
    }
    let side = 2 * halo as usize + 1;
    let [width, height] = frame.dimensions().map(|n| (n as usize).min(side));
    Ok(spatial_scratch_bytes(width * height)? <= MAX_SPATIAL_SCRATCH_BYTES)
}

fn spatial_scratch_bytes(samples: usize) -> Result<usize, String> {
    samples
        .checked_mul(6)
        .and_then(|n| n.checked_add(131072))
        .ok_or_else(|| "Mask scratch size overflow".into())
}

impl<'a> Plan<'a> {
    #[cfg(test)]
    pub(crate) fn new(
        frame: Frame,
        output: Region,
        depth: u16,
        steps: &'a [Step<'a>],
    ) -> Result<Self, String> {
        Self::with_quantization(frame, output, depth, steps, false)
    }
    fn with_quantization(
        frame: Frame,
        output: Region,
        depth: u16,
        steps: &'a [Step<'a>],
        quantized: bool,
    ) -> Result<Self, String> {
        let (halo, spatial) = stack_properties(depth, steps.iter().cloned())?;
        Ok(Self {
            frame,
            output,
            grid: None,
            depth,
            cached: quantized || spatial,
            halo,
            steps,
        })
    }
    fn sample_bounds(&self) -> [i32; 4] {
        let [l, t, r, b] = self.output.bounds();
        match self.grid {
            Some([xs, ys]) => [
                xs[l as usize],
                ys[t as usize],
                xs[(r - 1) as usize] + 1,
                ys[(b - 1) as usize] + 1,
            ],
            None => [l, t, r, b],
        }
    }
    pub(crate) fn input_bounds(&self) -> [i32; 4] {
        let [l, t, r, b] = self.sample_bounds();
        if !self.cached {
            return [l, t, r, b];
        }
        let [w, h] = self.frame.dimensions().map(|n| n as i32);
        [
            (l.clamp(0, w - 1) - self.halo).max(0),
            (t.clamp(0, h - 1) - self.halo).max(0),
            ((r - 1).clamp(0, w - 1) + 1 + self.halo).min(w),
            ((b - 1).clamp(0, h - 1) + 1 + self.halo).min(h),
        ]
    }
    fn fits(&self, limit: usize) -> Result<bool, String> {
        if !self.cached {
            return Ok(true);
        }
        let [l, t, r, b] = self.input_bounds();
        Ok(spatial_scratch_bytes((r - l) as usize * (b - t) as usize)? <= limit)
    }
    fn visit_regions(
        &self,
        limit: usize,
        visit: &mut impl FnMut(&Self) -> Result<(), String>,
    ) -> Result<(), String> {
        if self.fits(limit)? {
            return visit(self);
        }
        let [l, t, r, b] = self.output.bounds();
        let (width, height) = (r - l, b - t);
        if width == 1 && height == 1 {
            return Err("A single mask sample exceeds the regional scratch budget".into());
        }
        let [sl, st, sr, sb] = self.sample_bounds();
        let parts = if height == 1 || (width > 1 && sr - sl >= sb - st) {
            let middle = l + width / 2;
            [[l, t, middle, b], [middle, t, r, b]]
        } else {
            let middle = t + height / 2;
            [[l, t, r, middle], [l, middle, r, b]]
        };
        for bounds in parts {
            // Each output keeps the full stack halo, including across the split.
            let part = Self {
                output: Region::new(bounds).map_err(|e| e.to_string())?,
                ..*self
            };
            part.visit_regions(limit, visit)?;
        }
        Ok(())
    }
    fn visit_grid_regions(
        &self,
        rows: &[i32],
        limit: usize,
        visit: &mut impl FnMut(&Self) -> Result<(), String>,
    ) -> Result<(), String> {
        // Keep short gaps together to amortize paint pin preparation.
        let adjacent = |&a: &i32, &b: &i32| b - a <= 2 * (2 * self.halo + 1);
        let [left, _, right, _] = self.output.bounds();
        let mut top = 0;
        for ys in rows.chunk_by(adjacent) {
            let bottom = top + ys.len() as i32;
            let part = Self {
                output: Region::new([left, top, right, bottom]).map_err(|e| e.to_string())?,
                ..*self
            };
            part.visit_regions(limit, visit)?;
            top = bottom;
        }
        Ok(())
    }
    fn render_bounded(
        &self,
        budget: &ResidentBudget,
        limit: usize,
        checks: &mut ReadChecks<'_>,
        mut render: impl FnMut(&Self, &mut ReadChecks<'_>) -> Result<RegionalMask, String>,
    ) -> Result<RegionalMask, String> {
        if self.grid.is_none() && self.fits(limit)? {
            return render(self, checks);
        }
        let bounds = self.output.bounds();
        let stride = usize::from(self.depth / 8);
        let row_bytes = (bounds[2] - bounds[0]) as usize * stride;
        let length = self.output.pixels() * stride;
        let mut bytes = ScratchBytes::new(budget, length, length).map_err(|e| e.to_string())?;
        let mut visit = |part: &Self| {
            checks.full()?;
            let result = render(part, checks)?;
            let [l, t, r, _] = part.output.bounds();
            let part_row_bytes = (r - l) as usize * stride;
            let start = (t - bounds[1]) as usize * row_bytes + (l - bounds[0]) as usize * stride;
            for (row, samples) in result.bytes.chunks_exact(part_row_bytes).enumerate() {
                let at = start + row * row_bytes;
                bytes[at..at + part_row_bytes].copy_from_slice(samples);
            }
            Ok(())
        };
        if let Some([_, rows]) = self.grid {
            self.visit_grid_regions(rows, limit, &mut visit)?;
        } else {
            self.visit_regions(limit, &mut visit)?;
        }
        checks.full()?;
        Ok(RegionalMask {
            bounds,
            depth: self.depth,
            fractional: false,
            bytes: bytes.freeze(),
        })
    }
    pub(crate) fn paint_steps(&self) -> impl Iterator<Item = usize> + '_ {
        self.steps
            .iter()
            .enumerate()
            .filter(|(_, s)| s.paints())
            .map(|(i, _)| i)
    }
    pub(crate) fn render(
        &self,
        budget: &ResidentBudget,
        checks: &mut ReadChecks<'_>,
        mut paint: impl FnMut(usize, i32, i32) -> Result<Pixel16, String>,
    ) -> Result<RegionalMask, String> {
        checks.full()?;
        if !self.cached {
            return self.render_pointwise(budget, checks, paint);
        }
        let [left, top, right, bottom] = self.input_bounds();
        let (width, height) = ((right - left) as usize, (bottom - top) as usize);
        let count = width * height;
        let bytes = spatial_scratch_bytes(count)?;
        let _charge = budget
            .reserve_bytes(bytes, MAX_SPATIAL_SCRATCH_BYTES)
            .map_err(|e| e.to_string())?;
        let maximum = if self.depth == 16 { 65535u16 } else { 255u16 };
        let mut values = filled(count, maximum)?;
        let mut scratch = filled(count, 0)?;
        for (index, step) in self
            .steps
            .iter()
            .enumerate()
            .filter(|(_, s)| s.enabled && s.weight > 0.)
        {
            checks.pinned()?;
            let before = if step.weight < 1. {
                Some(copied(&values)?)
            } else {
                None
            };
            match step.kind {
                "fill" => values.fill(if self.depth == 16 {
                    (f64::from(step.value) * 257.).round().clamp(0., 65535.) as u16
                } else {
                    step.value.round().clamp(0., 255.) as u16
                }),
                "paint" => {
                    for y in 0..height {
                        checks.pinned()?;
                        for x in 0..width {
                            let p = paint(index, x as i32 + left, y as i32 + top)?;
                            let (red, alpha) = if self.depth == 16 {
                                (p[0], p[3])
                            } else {
                                (u16::from(project16(p[0])), u16::from(project16(p[3])))
                            };
                            let at = y * width + x;
                            values[at] =
                                ((u64::from(values[at]) * (u64::from(maximum) - u64::from(alpha))
                                    + u64::from(red) * u64::from(alpha)
                                    + u64::from(maximum) / 2)
                                    / u64::from(maximum)) as u16;
                        }
                    }
                }
                "invert" => {
                    for v in &mut values {
                        *v = maximum - *v;
                    }
                }
                "density" => {
                    for value in &mut values {
                        *value = crate::mask::density_sample(f64::from(*value), step.value, maximum);
                    }
                }
                "levels" => {
                    for v in &mut values {
                        *v = if self.depth == 16 {
                            ((f64::from(*v) / 65535.).powf(f64::from(step.value.clamp(0.1, 5.)))
                                * 65535.)
                                .round()
                                .clamp(0., 65535.) as u16
                        } else {
                            ((f32::from(*v) / 255.).powf(step.value.clamp(0.1, 5.)) * 255.).round()
                                as u16
                        };
                    }
                }
                "curves" | "adjust" => {
                    let mut table = Vec::new();
                    table
                        .try_reserve_exact(usize::from(maximum) + 1)
                        .map_err(|_| "Mask lookup allocation failed")?;
                    for v in 0..=maximum {
                        table.push(if self.depth == 16 {
                            (crate::depth16::color::map(
                                &step.kind,
                                step.settings,
                                f64::from(v) / 65535.,
                            ) * 65535.)
                                .round()
                                .clamp(0., 65535.) as u16
                        } else {
                            (effects::map_value(step.kind, step.settings, f32::from(v) / 255.)
                                * 255.)
                                .round() as u16
                        });
                    }
                    for v in &mut values {
                        *v = table[usize::from(*v)];
                    }
                }
                "gaussian" | "blur" => {
                    let radii = if step.kind == "gaussian" {
                        effects::gaussian_radii(effects::number(step.settings, "radius", 8.))
                    } else if step.value >= 0.5 {
                        [(step.value.round().clamp(1., 64.) as usize).div_ceil(3); 3]
                    } else {
                        [0; 3]
                    };
                    for radius in radii {
                        if radius == 0 {
                            continue;
                        }
                        box_pass(&values, &mut scratch, width, height, radius, false, &mut || {
                            checks.pinned()
                        })?;
                        box_pass(&scratch, &mut values, width, height, radius, true, &mut || {
                            checks.pinned()
                        })?;
                    }
                }
                _ => return Err("Unsupported regional mask stage".into()),
            }
            if let Some(before) = before {
                for (v, old) in values.iter_mut().zip(before) {
                    *v = (f64::from(old) * (1. - f64::from(step.weight))
                        + f64::from(*v) * f64::from(step.weight))
                    .round()
                    .clamp(0., f64::from(maximum)) as u16;
                }
            }
        }
        let depth = self.depth;
        let length = self.output.pixels() * if depth == 16 { 2 } else { 1 };
        let mut output = ScratchBytes::new(budget, length, length).map_err(|e| e.to_string())?;
        let mut at = 0;
        let b = self.output.bounds();
        let [fw, fh] = self.frame.dimensions().map(|n| n as i32);
        for y in b[1]..b[3] {
            checks.pinned()?;
            let sy = self.grid.map_or_else(|| y.clamp(0, fh - 1), |[_, ys]| ys[y as usize]);
            for x in b[0]..b[2] {
                let sx = self.grid.map_or_else(|| x.clamp(0, fw - 1), |[xs, _]| xs[x as usize]);
                let v = values[(sy - top) as usize * width + (sx - left) as usize];
                if depth == 16 {
                    output[at..at + 2].copy_from_slice(&v.to_be_bytes());
                    at += 2;
                } else {
                    output[at] = v as u8;
                    at += 1;
                }
            }
        }
        checks.full()?;
        Ok(RegionalMask {
            bounds: b,
            depth,
            fractional: false,
            bytes: output.freeze(),
        })
    }
    fn render_pointwise(
        &self,
        budget: &ResidentBudget,
        checks: &mut ReadChecks<'_>,
        mut paint: impl FnMut(usize, i32, i32) -> Result<Pixel16, String>,
    ) -> Result<RegionalMask, String> {
        let stride = usize::from(self.depth / 2);
        let length = self.output.pixels() * stride;
        let mut bytes = ScratchBytes::new(budget, length, length).map_err(|e| e.to_string())?;
        let mut at = 0;
        let b = self.output.bounds();
        for y in b[1]..b[3] {
            checks.pinned()?;
            for x in b[0]..b[2] {
                self.write_pointwise(x, y, &mut bytes[at..at + stride], &mut paint)?;
                at += stride;
            }
        }
        checks.full()?;
        Ok(RegionalMask {
            bounds: b,
            depth: self.depth,
            fractional: true,
            bytes: bytes.freeze(),
        })
    }

    fn write_pointwise(
        &self,
        x: i32,
        y: i32,
        output: &mut [u8],
        paint: &mut impl FnMut(usize, i32, i32) -> Result<Pixel16, String>,
    ) -> Result<(), String> {
        let steps = self
            .steps
            .iter()
            .enumerate()
            .filter(|(_, step)| step.enabled && step.weight > 0.);
        if self.depth == 16 {
            let mut value = 65535.0f64;
            for (index, s) in steps {
                let before = value;
                match s.kind {
                    "fill" => value = f64::from(s.value) * 257.,
                    "paint" => {
                        let p = paint(index, x, y)?;
                        let a = f64::from(p[3]) / 65535.;
                        value = value * (1. - a) + f64::from(p[0]) * a;
                    }
                    "invert" => value = 65535. - value,
                    "density" => value = f64::from(crate::mask::density_sample(value, s.value, 65535)),
                    "levels" => {
                        value = (value / 65535.).powf(f64::from(s.value.clamp(0.1, 5.))) * 65535.
                    }
                    _ => {}
                }
                value = before * (1. - f64::from(s.weight)) + value * f64::from(s.weight);
            }
            let value = value.clamp(0., 65535.) / 65535.;
            if !value.is_finite() {
                return Err("Pointwise native16 mask produced a non-finite factor".into());
            }
            output.copy_from_slice(&value.to_bits().to_be_bytes());
        } else {
            let mut value = 255.0f32;
            for (index, s) in steps {
                let before = value;
                match s.kind {
                    "fill" => value = s.value,
                    "paint" => {
                        let p = paint(index, x, y)?.map(project16);
                        let a = f32::from(p[3]) / 255.;
                        value = value * (1. - a) + f32::from(p[0]) * a;
                    }
                    "invert" => value = 255. - value,
                    "density" => value = f32::from(crate::mask::density_sample(f64::from(value), s.value, 255)),
                    "levels" => value = ((value / 255.).powf(s.value.clamp(0.1, 5.)) * 255.).clamp(0., 255.),
                    _ => {}
                }
                value = before * (1. - s.weight) + value * s.weight;
            }
            let value = value.clamp(0., 255.) / 255.;
            if !value.is_finite() {
                return Err("Pointwise native8 mask produced a non-finite factor".into());
            }
            output.copy_from_slice(&value.to_bits().to_be_bytes());
        }
        Ok(())
    }
}
fn filled(count: usize, value: u16) -> Result<Vec<u16>, String> {
    let mut result = Vec::new();
    result
        .try_reserve_exact(count)
        .map_err(|_| "Mask buffer allocation failed")?;
    result.resize(count, value);
    Ok(result)
}
fn copied(source: &[u16]) -> Result<Vec<u16>, String> {
    let mut result = Vec::new();
    result
        .try_reserve_exact(source.len())
        .map_err(|_| "Mask weight allocation failed")?;
    result.extend_from_slice(source);
    Ok(result)
}
fn box_pass(
    source: &[u16],
    target: &mut [u16],
    width: usize,
    height: usize,
    radius: usize,
    vertical: bool,
    check: &mut dyn FnMut() -> Result<(), String>,
) -> Result<(), String> {
    let (lines, length, stride) = if vertical {
        (width, height, width)
    } else {
        (height, width, 1)
    };
    let n = (radius * 2 + 1) as u64;
    for line in 0..lines {
        if line % 64 == 0 {
            check()?;
        }
        let base = if vertical { line } else { line * width };
        let at =
            |i: isize| u64::from(source[base + i.clamp(0, length as isize - 1) as usize * stride]);
        let mut sum: u64 = (-(radius as isize)..=radius as isize).map(at).sum();
        for i in 0..length {
            target[base + i * stride] = ((sum + n / 2) / n) as u16;
            sum = sum + at(i as isize + radius as isize + 1) - at(i as isize - radius as isize);
        }
    }
    Ok(())
}
/// Validate mask stages and their captured paint sources before skipping empty color windows.
pub(crate) fn validate_scene_sources(
    scene: &crate::engine::scene::SceneSnapshot,
    layer: crate::engine::native::LayerMetadata<'_>,
    check: &mut dyn FnMut() -> Result<(), String>,
) -> Result<(), String> {
    let Some(mask) = layer.mask() else { return Ok(()) };
    check()?;
    Frame::new(layer.pixels().width(), layer.pixels().height())?;
    stack_properties(scene.metadata().depth(), mask.steps().map(Step::from))?;
    for step in mask.steps().filter(|&step| Step::from(step).paints()) {
        check()?;
        let source = scene.raster(step.pixels().slot())?;
        check_paint_admission(source, scene.budget(), scene.disk_budget())?;
    }
    check()
}

/// Prepare the raw layer-local mask. The enabled flag controls composition,
/// but must never erase disabled mask samples in a Photoshop channel.
pub(crate) fn prepare_scene(
    scene: &crate::engine::scene::SceneSnapshot,
    layer: crate::engine::native::LayerMetadata<'_>,
    bounds: [i32; 4],
    check: &mut dyn FnMut() -> Result<(), String>,
) -> Result<RegionalMask, String> {
    prepare_scene_prefix(scene, layer, bounds, None, check)
}

pub(crate) fn prepare_scene_prefix(
    scene: &crate::engine::scene::SceneSnapshot,
    layer: crate::engine::native::LayerMetadata<'_>,
    bounds: [i32; 4],
    prefix: Option<usize>,
    check: &mut dyn FnMut() -> Result<(), String>,
) -> Result<RegionalMask, String> {
    prepare_scene_with_checks(
        scene,
        layer,
        bounds,
        prefix,
        &mut ReadChecks::caller_only(check),
    )
}

pub(crate) fn prepare_scene_with_checks(
    scene: &crate::engine::scene::SceneSnapshot,
    layer: crate::engine::native::LayerMetadata<'_>,
    bounds: [i32; 4],
    prefix: Option<usize>,
    checks: &mut ReadChecks<'_>,
) -> Result<RegionalMask, String> {
    prepare_source(
        layer,
        bounds,
        scene.metadata().depth(),
        scene.budget(),
        scene.disk_budget(),
        checks,
        |slot| scene.raster(slot),
        prefix.map(|count| (count, false)),
    )
}
pub(crate) fn prepare_native(
    document: &crate::engine::native::NativeDocument,
    layer: crate::engine::native::LayerMetadata<'_>,
    bounds: [i32; 4],
    check: &mut dyn FnMut() -> Result<(), String>,
) -> Result<RegionalMask, String> {
    if document.layer_slot(layer.id())? != layer.pixels().slot() {
        return Err("Mask layer differs from its captured native document".into());
    }
    prepare_source(
        layer,
        bounds,
        layer.pixels().depth(),
        document.budget(),
        document.disk_budget(),
        &mut ReadChecks::caller_only(check),
        |slot| document.source(slot),
        None,
    )
}
pub(crate) fn prepare_bake(
    document: &crate::engine::native::NativeDocument,
    layer: crate::engine::native::LayerMetadata<'_>,
    bounds: [i32; 4],
    prefix: usize,
    quantized: bool,
    checks: &mut ReadChecks<'_>,
) -> Result<RegionalMask, String> {
    check_bake_source(document, layer, prefix)?;
    prepare_source(
        layer,
        bounds,
        layer.pixels().depth(),
        document.budget(),
        document.disk_budget(),
        checks,
        |slot| document.source(slot),
        Some((prefix, quantized)),
    )
}

fn check_bake_source(
    document: &crate::engine::native::NativeDocument,
    layer: crate::engine::native::LayerMetadata<'_>,
    prefix: usize,
) -> Result<(), String> {
    if document.layer_slot(layer.id())? != layer.pixels().slot() {
        return Err("Mask layer differs from its captured native document".into());
    }
    if prefix == 0 {
        return Err("Mask bake prefix differs from its captured stack".into());
    }
    Ok(())
}

fn mask_steps<'a>(
    mask: crate::engine::native::MaskMetadata<'a>,
    prefix: Option<usize>,
    budget: &ResidentBudget,
) -> Result<(Reservation, Vec<Step<'a>>), String> {
    let count = prefix.unwrap_or(mask.steps().len());
    if count > mask.steps().len() {
        return Err("Mask bake prefix differs from its captured stack".into());
    }
    if count > 64 {
        return Err("Too many regional mask stages".into());
    }
    let charge = budget
        .reserve_bytes(512 + count * 256, 32 * 1024)
        .map_err(|e| e.to_string())?;
    Ok((charge, mask.steps().take(count).map(Step::from).collect()))
}

/// Sample a mask prefix at ordered layer-local positions. The output
/// coordinates index the supplied grid.
pub(crate) fn prepare_bake_grid(
    document: &crate::engine::native::NativeDocument,
    layer: crate::engine::native::LayerMetadata<'_>,
    prefix: usize,
    columns: &[i32],
    rows: &[i32],
    checks: &mut ReadChecks<'_>,
) -> Result<RegionalMask, String> {
    checks.full()?;
    check_bake_source(document, layer, prefix)?;
    let mask = layer.mask().ok_or("Native layer has no mask channel")?;
    let budget = document.budget();
    let (_metadata, steps) = mask_steps(mask, Some(prefix), budget)?;
    let frame = Frame::new(layer.pixels().width(), layer.pixels().height())?;
    for (samples, limit) in [columns, rows].into_iter().zip(frame.dimensions()) {
        if samples.first().is_none_or(|&first| first < 0)
            || samples.last().is_none_or(|&last| last >= limit as i32)
            || samples.windows(2).any(|pair| pair[0] >= pair[1])
        {
            return Err("Mask grid positions must be ordered and inside the layer".into());
        }
    }
    let bounds = [0, 0, columns.len() as i32, rows.len() as i32];
    let output = Region::new(bounds).map_err(|error| error.to_string())?;
    let depth = layer.pixels().depth();
    let plan = Plan::with_quantization(frame, output, depth, &steps, false)?;
    if plan.cached {
        let plan = Plan {
            grid: Some([columns, rows]),
            ..plan
        };
        return plan.render_bounded(budget, MAX_SPATIAL_SCRATCH_BYTES, checks, |part, checks| {
            prepare_plan(part, mask, budget, document.disk_budget(), checks, &|slot| {
                document.source(slot)
            })
        });
    }
    let mut sources: [Option<&Surface>; 64] = [None; 64];
    for index in plan.paint_steps() {
        let step = mask.steps().nth(index).ok_or("Mask stage is missing")?;
        let surface = document.source(step.pixels().slot())?;
        check_paint_admission(surface, budget, document.disk_budget())?;
        if !surface.is_known_zero() {
            sources[index] = Some(surface);
        }
    }

    let stride = usize::from(depth / 2);
    let row_bytes = columns.len() * stride;
    let length = output.pixels() * stride;
    let mut bytes = ScratchBytes::new(budget, length, length).map_err(|error| error.to_string())?;
    let same_tile = |&a: &i32, &b: &i32| a / TILE as i32 == b / TILE as i32;
    let mut top = 0;
    for ys in rows.chunk_by(same_tile) {
        let mut left = 0;
        for xs in columns.chunk_by(same_tile) {
            let area = Region::new([xs[0], ys[0], xs[xs.len() - 1] + 1, ys[ys.len() - 1] + 1])
                .map_err(|error| error.to_string())?;
            let mut pins: [Option<SurfaceRegion>; 64] = [const { None }; 64];
            for (index, source) in sources
                .iter()
                .enumerate()
                .filter_map(|(index, source)| source.map(|surface| (index, surface)))
            {
                pins[index] = Some(checks.surface_region(source, area)?);
            }
            let mut paint = |index: usize, x, y| match &pins[index] {
                Some(pin) => pin.pixel16(x, y).map_err(|error| error.to_string()),
                // Active paint stages without a pin are explicit zero surfaces.
                None => Ok([0; 4]),
            };
            for (row, &y) in ys.iter().enumerate() {
                checks.pinned()?;
                for (column, &x) in xs.iter().enumerate() {
                    let at = (top + row) * row_bytes + (left + column) * stride;
                    plan.write_pointwise(x, y, &mut bytes[at..at + stride], &mut paint)?;
                }
            }
            left += xs.len();
        }
        top += ys.len();
    }
    checks.full()?;
    Ok(RegionalMask {
        bounds,
        depth,
        fractional: true,
        bytes: bytes.freeze(),
    })
}
fn prepare_source<'a>(
    layer: crate::engine::native::LayerMetadata<'_>,
    bounds: [i32; 4],
    depth: u16,
    budget: &ResidentBudget,
    disk: &crate::large_document::DiskBudget,
    checks: &mut ReadChecks<'_>,
    source: impl Fn(crate::engine::native::RasterSlot) -> Result<&'a crate::raster::Surface, String>,
    prefix: Option<(usize, bool)>,
) -> Result<RegionalMask, String> {
    checks.full()?;
    let [l, t, r, b] = bounds;
    let [ox, oy] = layer.origin();
    let local = |v: i32, origin| v.checked_sub(origin).ok_or("Native mask offset overflow");
    let output = Region::new([local(l, ox)?, local(t, oy)?, local(r, ox)?, local(b, oy)?])
        .map_err(|e| e.to_string())?;
    let mask = layer.mask().ok_or("Native layer has no mask channel")?;
    let (_metadata, steps) = mask_steps(mask, prefix.map(|p| p.0), budget)?;
    let frame = Frame::new(layer.pixels().width(), layer.pixels().height())?;
    let plan = Plan::with_quantization(frame, output, depth, &steps, prefix.is_some_and(|p| p.1))?;
    plan.render_bounded(budget, MAX_SPATIAL_SCRATCH_BYTES, checks, |part, checks| {
        prepare_plan(part, mask, budget, disk, checks, &source)
    })
}

fn prepare_plan<'a>(
    plan: &Plan<'_>,
    mask: crate::engine::native::MaskMetadata<'_>,
    budget: &ResidentBudget,
    disk: &crate::large_document::DiskBudget,
    checks: &mut ReadChecks<'_>,
    source: &impl Fn(crate::engine::native::RasterSlot) -> Result<&'a crate::raster::Surface, String>,
) -> Result<RegionalMask, String> {
    let [left, top, right, bottom] = plan.input_bounds();
    let band_height = (MAX_REGION_PIXELS / (right - left) as usize).max(1);
    let bands = ((bottom - top) as usize).div_ceil(band_height);
    let slots = plan
        .paint_steps()
        .count()
        .checked_mul(bands)
        .ok_or("Mask paint footprint count overflow")?;
    let _pins = budget
        .reserve_bytes(
            slots
                .checked_mul(std::mem::size_of::<crate::raster::SurfaceRegion>())
                .ok_or("Mask paint footprint size overflow")?,
            16 * 1024 * 1024,
        )
        .map_err(|e| e.to_string())?;
    let mut pins = Vec::new();
    pins.try_reserve_exact(slots)
        .map_err(|_| "Mask paint index allocation refused")?;
    let mut bases = [None; 64];
    for i in plan.paint_steps() {
        let step = mask.steps().nth(i).ok_or("Mask stage is missing")?;
        let surface = source(step.pixels().slot())?;
        check_paint_admission(surface, budget, disk)?;
        bases[i] = Some(pins.len());
        for y in (top..bottom).step_by(band_height) {
            let region = Region::new([left, y, right, (y + band_height as i32).min(bottom)])
                .map_err(|e| e.to_string())?;
            pins.push(checks.surface_region(surface, region)?);
        }
    }

    // Rendering uses only the immutable paint pins and admitted mask buffers.
    plan.render(budget, checks, |i, x, y| {
        let offset = usize::try_from(y - top).map_err(|_| "Mask paint sample precedes footprint")?
            / band_height;
        let base = bases[i].ok_or("Mask paint stage was not prepared")?;
        if offset >= bands {
            return Err("Mask paint sample exceeds footprint".into());
        }
        pins[base + offset].pixel16(x, y).map_err(|e| e.to_string())
    })
}

fn check_paint_admission(
    source: &crate::raster::Surface,
    budget: &ResidentBudget,
    disk: &crate::large_document::DiskBudget,
) -> Result<(), String> {
    if !source.uses_budget(budget) || !source.uses_disk_budget(disk) {
        return Err("Mask paint source differs from captured admission".into());
    }
    Ok(())
}

#[derive(Clone)]
pub(crate) struct RegionalMask {
    bounds: [i32; 4],
    depth: u16,
    fractional: bool,
    bytes: PinnedBytes,
}
impl RegionalMask {
    pub(crate) fn bounds(&self) -> [i32; 4] {
        self.bounds
    }
    pub(crate) fn depth(&self) -> u16 {
        self.depth
    }
    pub(crate) fn uses_budget(&self, budget: &ResidentBudget) -> bool {
        self.bytes.uses_budget(budget)
    }
    pub(crate) fn native(&self, x: i32, y: i32) -> Result<u16, String> {
        if self.fractional {
            return Err(
                "Pointwise mask has a fractional factor; use its native float sampler".into(),
            );
        }
        if x < self.bounds[0] || y < self.bounds[1] || x >= self.bounds[2] || y >= self.bounds[3] {
            return Err("Mask sample is outside its prepared output".into());
        }
        let at = (y - self.bounds[1]) as usize * (self.bounds[2] - self.bounds[0]) as usize
            + (x - self.bounds[0]) as usize;
        Ok(if self.depth == 16 {
            u16::from_be_bytes([self.bytes[at * 2], self.bytes[at * 2 + 1]])
        } else {
            u16::from(self.bytes[at])
        })
    }
    pub(crate) fn value8(&self, x: i32, y: i32) -> Result<f32, String> {
        if self.fractional {
            return Ok(self.factor(x, y)? as f32);
        }
        Ok(f32::from(self.native(x, y)?) / if self.depth == 16 { 65535. } else { 255. })
    }
    pub(crate) fn value16(&self, x: i32, y: i32) -> Result<f64, String> {
        if self.fractional {
            return self.factor(x, y);
        }
        Ok(f64::from(self.native(x, y)?) / if self.depth == 16 { 65535. } else { 255. })
    }
    fn factor(&self, x: i32, y: i32) -> Result<f64, String> {
        if x < self.bounds[0] || y < self.bounds[1] || x >= self.bounds[2] || y >= self.bounds[3] {
            return Err("Mask factor is outside its prepared output".into());
        }
        let at = (y - self.bounds[1]) as usize * (self.bounds[2] - self.bounds[0]) as usize
            + (x - self.bounds[0]) as usize;
        Ok(if self.depth == 16 {
            f64::from_bits(u64::from_be_bytes(
                self.bytes[at * 8..at * 8 + 8].try_into().unwrap(),
            ))
        } else {
            f64::from(f32::from_bits(u32::from_be_bytes(
                self.bytes[at * 4..at * 4 + 4].try_into().unwrap(),
            )))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        engine::{Document, Mask, MaskStep},
        raster::Raster,
    };
    use serde_json::json;
    fn fixture(depth: u16) -> Document {
        let (w, h) = (129, 133);
        let mut doc = Document::new_depth(w, h, depth).unwrap();
        let mut paint = Raster::new_depth(w, h, depth);
        for y in 0..h {
            for x in 0..w {
                let p = [
                    10001 + (x * 317 + y * 173) % 55001,
                    0,
                    0,
                    if (x + y) % 7 == 0 {
                        0
                    } else {
                        18001 + (x * 137 + y * 379) % 47535
                    },
                ]
                .map(|v| v as u16);
                paint.set16(x as i32, y as i32, p);
            }
        }
        let values = [
            ("fill", 173.7, 0.91),
            ("paint", 0., 0.77),
            ("gaussian", 0., 0.7),
            ("invert", 0., 0.43),
            ("blur", 3.7, 0.81),
            ("levels", 1.9, 0.63),
            ("curves", 0., 0.77),
            ("adjust", 0., 0.56),
        ];
        let steps = values
            .into_iter()
            .enumerate()
            .map(|(i, (kind, value, weight))| MaskStep {
                id: format!("step-{i}"),
                kind: kind.into(),
                enabled: true,
                weight,
                value,
                pixels: if kind == "paint" {
                    paint.clone()
                } else {
                    Raster::new_depth(w, h, depth)
                },
                settings: match kind {
                    "gaussian" => json!({"radius":1.3}),
                    "curves" => effects::defaults("curves"),
                    "adjust" => json!({"brightness":0.13,"contrast":1.23}),
                    _ => json!({}),
                },
            })
            .collect();
        doc.layers[0].mask = Some(Mask {
            enabled: true,
            linked: true,
            steps,
            cache_key: crate::engine::id(),
        });
        doc
    }
    #[test]
    fn native_mask_regions_match_existing_weighted_full_frame_kernels() {
        for depth in [8, 16] {
            let doc = fixture(depth);
            let mask = doc.layers[0].mask.as_ref().unwrap();
            let steps: Vec<_> = mask.steps.iter().map(Step::from).collect();
            let original = mask.steps[1].pixels.rgba16();
            for area in [[49, 43, 80, 76], [0, 0, 13, 17], [113, 117, 129, 133]] {
                let output = Region::new(area).unwrap();
                let plan = Plan::new(
                    Frame::new(doc.width, doc.height).unwrap(),
                    output,
                    depth,
                    &steps,
                )
                .unwrap();
                let budget = ResidentBudget::new(16 * 1024 * 1024).unwrap();
                let result = plan
                    .render(&budget, &mut ReadChecks::caller_only(&mut || Ok(())), |i, x, y| {
                        Ok(mask.steps[i].pixels.get16(x, y))
                    })
                    .unwrap();
                if depth == 16 {
                    let full = crate::depth16::prepare_masks(&doc).unwrap();
                    let full = full[0].as_ref().unwrap();
                    for y in area[1]..area[3] {
                        for x in area[0]..area[2] {
                            assert_eq!(
                                result.native(x, y).unwrap(),
                                full.get(x, y),
                                "native16({x},{y})"
                            );
                        }
                    }
                } else {
                    let full = mask.prepare(doc.width, doc.height).unwrap();
                    for y in area[1]..area[3] {
                        for x in area[0]..area[2] {
                            assert_eq!(
                                result.value8(x, y).unwrap(),
                                full.value(x, y),
                                "native8({x},{y})"
                            );
                        }
                    }
                }
                assert!(result.native(area[0] - 1, area[1]).is_err());
                assert_eq!(
                    budget.used(),
                    output.pixels() * if depth == 16 { 2 } else { 1 }
                );
                let other = result.clone();
                drop(result);
                assert!(budget.used() > 0);
                drop(other);
                assert_eq!(budget.used(), 0);
            }
            assert_eq!(mask.steps[1].pixels.rgba16(), original);
        }
    }
    #[test]
    fn split_mask_regions_preserve_native_samples_and_discard_failed_output() {
        for depth in [8, 16] {
            let doc = fixture(depth);
            let mask = doc.layers[0].mask.as_ref().unwrap();
            let steps: Vec<_> = mask.steps.iter().map(Step::from).collect();
            let output = Region::new([-4, -2, 134, 138]).unwrap();
            let plan = Plan::new(
                Frame::new(doc.width, doc.height).unwrap(),
                output,
                depth,
                &steps,
            )
            .unwrap();
            let budget = ResidentBudget::new(4 * 1024 * 1024).unwrap();
            let limit = spatial_scratch_bytes(4096).unwrap();
            assert!(!plan.fits(limit).unwrap());
            let paint = |i: usize, x, y| Ok(mask.steps[i].pixels.get16(x, y));
            let whole = plan.render(&budget, &mut ReadChecks::caller_only(&mut || Ok(())), paint).unwrap();
            let mut regions = 0;
            let split = plan
                .render_bounded(&budget, limit, &mut ReadChecks::caller_only(&mut || Ok(())), |part, checks| {
                    regions += 1;
                    assert!(part.fits(limit).unwrap());
                    part.render(&budget, checks, paint)
                })
                .unwrap();
            assert!(regions > 1);
            assert_eq!(split.bounds(), whole.bounds());
            assert_eq!(&*split.bytes, &*whole.bytes);
            drop((split, whole));
            assert_eq!(budget.used(), 0);

            let mut regions = 0;
            let result = plan.render_bounded(&budget, limit, &mut ReadChecks::caller_only(&mut || Ok(())), |part, checks| {
                regions += 1;
                if regions == 2 {
                    return Err("Mask source read failed".into());
                }
                part.render(&budget, checks, paint)
            });
            assert!(result.is_err());
            assert_eq!(regions, 2);
            assert_eq!(budget.used(), 0);
        }
    }
    #[test]
    fn sampled_feather_regions_preserve_native_values_at_gaps_and_splits() {
        let columns = [0, 4, 31, 63, 77, 120, 128];
        let rows = [0, 2, 13, 65, 66, 80, 132];
        for depth in [8, 16] {
            let doc = fixture(depth);
            let mask = doc.layers[0].mask.as_ref().unwrap();
            let steps: Vec<_> = mask.steps.iter().map(Step::from).collect();
            let frame = Frame::new(doc.width, doc.height).unwrap();
            let full = Region::new([0, 0, doc.width as i32, doc.height as i32]).unwrap();
            let dense = Plan::new(frame, full, depth, &steps).unwrap();
            let budget = ResidentBudget::new(4 * 1024 * 1024).unwrap();
            let paint = |i: usize, x, y| Ok(mask.steps[i].pixels.get16(x, y));
            let mut check = || Ok(());
            let mut checks = ReadChecks::caller_only(&mut check);
            let expected = dense.render(&budget, &mut checks, paint).unwrap();
            let output = Region::new([0, 0, columns.len() as i32, rows.len() as i32]).unwrap();
            let plan = Plan { output, grid: Some([&columns, &rows]), ..dense };
            let limit = spatial_scratch_bytes(2300).unwrap();
            let mut windows = 0;
            let sampled = plan.render_bounded(&budget, limit, &mut checks, |part, checks| {
                windows += 1;
                let [_, top, _, bottom] = part.sample_bounds();
                assert!(bottom - top <= 16);
                assert!(part.fits(limit).unwrap());
                part.render(&budget, checks, paint)
            }).unwrap();
            assert!(windows > 3);
            for (y, &sy) in rows.iter().enumerate() {
                for (x, &sx) in columns.iter().enumerate() {
                    assert_eq!(sampled.native(x as i32, y as i32).unwrap(), expected.native(sx, sy).unwrap());
                }
            }
        }
    }
    #[test]
    fn bake_prefix_keeps_quantization_required_by_later_stages() {
        for depth in [8, 16] {
            let doc = fixture(depth);
            let mut mask = doc.layers[0].mask.as_ref().unwrap().clone();
            assert!(mask.needs_cache());
            mask.steps.truncate(2);
            assert!(!mask.needs_cache());
            let steps: Vec<_> = mask.steps.iter().map(Step::from).collect();
            let budget = ResidentBudget::new(4 * 1024 * 1024).unwrap();
            for area in [[49, 43, 80, 76], [-11, -7, 19, 13], [120, 125, 146, 151]] {
                let result = Plan::with_quantization(
                    Frame::new(doc.width, doc.height).unwrap(),
                    Region::new(area).unwrap(),
                    depth,
                    &steps,
                    true,
                )
                .unwrap()
                .render(&budget, &mut ReadChecks::caller_only(&mut || Ok(())), |i, x, y| {
                    Ok(mask.steps[i].pixels.get16(x, y))
                })
                .unwrap();
                if depth == 16 {
                    let full =
                        crate::depth16::mask::evaluate(&mask, doc.width, doc.height).unwrap();
                    for y in area[1]..area[3] {
                        for x in area[0]..area[2] {
                            assert_eq!(result.native(x, y).unwrap(), full.get(x, y));
                        }
                    }
                } else {
                    let full = crate::mask::evaluate(&mask, doc.width, doc.height);
                    for y in area[1]..area[3] {
                        for x in area[0]..area[2] {
                            assert_eq!(result.value8(x, y).unwrap(), full.value(x, y));
                        }
                    }
                }
                drop(result);
                assert_eq!(budget.used(), 0);
            }
        }
    }
    #[test]
    fn mask_faults_capacity_and_cancellation_publish_no_partial_pixels() {
        let doc = fixture(16);
        let mask = doc.layers[0].mask.as_ref().unwrap();
        let steps: Vec<_> = mask.steps.iter().map(Step::from).collect();
        let plan = Plan::new(
            Frame::new(doc.width, doc.height).unwrap(),
            Region::new([49, 43, 80, 76]).unwrap(),
            16,
            &steps,
        )
        .unwrap();
        let original = mask.steps[1].pixels.rgba16();
        let budget = ResidentBudget::new(16 * 1024 * 1024).unwrap();
        let mut reads = 0;
        assert!(plan
            .render(&budget, &mut ReadChecks::caller_only(&mut || Ok(())), |_, _, _| {
                reads += 1;
                Err("Synthetic native paint read failure".into())
            })
            .is_err());
        assert_eq!(reads, 1);
        assert_eq!(budget.used(), 0);
        assert!(plan
            .render(
                &budget,
                &mut ReadChecks::caller_only(&mut || Err("Synthetic cancellation".into())),
                |_, _, _| panic!("Cancelled mask must not sample")
            )
            .is_err());
        assert_eq!(budget.used(), 0);
        assert!(plan
            .render(
                &ResidentBudget::new(1).unwrap(),
                &mut ReadChecks::caller_only(&mut || Ok(())),
                |_, _, _| panic!("Refused mask must not sample")
            )
            .is_err());
        assert_eq!(mask.steps[1].pixels.rgba16(), original);
    }
    #[test]
    fn pointwise_masks_keep_fractional_native_factor_bits_and_outside_paint_rules() {
        for depth in [8, 16] {
            let mut doc = fixture(depth);
            let mask = doc.layers[0].mask.as_mut().unwrap();
            mask.steps
                .retain(|s| ["fill", "paint", "invert", "levels"].contains(&s.kind.as_str()));
            let mask = doc.layers[0].mask.as_ref().unwrap();
            assert!(!mask.needs_cache());
            let steps: Vec<_> = mask.steps.iter().map(Step::from).collect();
            for b in [[49, 43, 80, 76], [-11, -7, 19, 13], [120, 125, 146, 151]] {
                let budget = ResidentBudget::new(4 * 1024 * 1024).unwrap();
                let plan = Plan::new(
                    Frame::new(doc.width, doc.height).unwrap(),
                    Region::new(b).unwrap(),
                    depth,
                    &steps,
                )
                .unwrap();
                let result = plan
                    .render(&budget, &mut ReadChecks::caller_only(&mut || Ok(())), |i, x, y| {
                        Ok(mask.steps[i].pixels.get16(x, y))
                    })
                    .unwrap();
                for y in b[1]..b[3] {
                    for x in b[0]..b[2] {
                        if depth == 16 {
                            assert_eq!(
                                result.value16(x, y).unwrap().to_bits(),
                                crate::depth16::mask::value(&doc.layers[0], x, y, None, false)
                                    .to_bits(),
                                "native16({x},{y})"
                            );
                        } else {
                            assert_eq!(
                                result.value8(x, y).unwrap().to_bits(),
                                doc.layers[0]
                                    .mask_value_prepared(x, y, None, false)
                                    .to_bits(),
                                "native8({x},{y})"
                            );
                        }
                    }
                }
                assert!(result.native(b[0], b[1]).is_err());
                assert_eq!(
                    budget.used(),
                    Region::new(b).unwrap().pixels() * if depth == 16 { 8 } else { 4 }
                );
                drop(result);
                assert_eq!(budget.used(), 0);
            }
        }
    }
    #[test]
    fn cached_masks_keep_true_frame_edge_clamping_for_declared_outside_outputs() {
        for depth in [8, 16] {
            let doc = fixture(depth);
            let mask = doc.layers[0].mask.as_ref().unwrap();
            let steps: Vec<_> = mask.steps.iter().map(Step::from).collect();
            for b in [[-11, -7, 19, 13], [120, 125, 146, 151]] {
                let budget = ResidentBudget::new(16 * 1024 * 1024).unwrap();
                let result = Plan::new(
                    Frame::new(doc.width, doc.height).unwrap(),
                    Region::new(b).unwrap(),
                    depth,
                    &steps,
                )
                .unwrap()
                .render(&budget, &mut ReadChecks::caller_only(&mut || Ok(())), |i, x, y| {
                    Ok(mask.steps[i].pixels.get16(x, y))
                })
                .unwrap();
                if depth == 16 {
                    let masks = crate::depth16::prepare_masks(&doc).unwrap();
                    let full = masks[0].as_ref().unwrap();
                    for y in b[1]..b[3] {
                        for x in b[0]..b[2] {
                            assert_eq!(
                                result.value16(x, y).unwrap().to_bits(),
                                full.value(x, y).to_bits()
                            );
                        }
                    }
                } else {
                    let full = mask.prepare(doc.width, doc.height).unwrap();
                    for y in b[1]..b[3] {
                        for x in b[0]..b[2] {
                            assert_eq!(
                                result.value8(x, y).unwrap().to_bits(),
                                full.value(x, y).to_bits()
                            );
                        }
                    }
                }
                drop(result);
                assert_eq!(budget.used(), 0);
            }
        }
    }
}
