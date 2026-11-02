//! Native mask stages consume explicitly prepared paint footprints. No full
//! canvas mask, implicit source read or disposable-source fallback is involved.
use crate::{
    effects::{self, regional::Frame},
    large_document::{PinnedBytes, ResidentBudget, ScratchBytes},
    raster::{
        backed::{Region, MAX_REGION_PIXELS},
        project16, Pixel16,
    },
};
use serde_json::Value;

#[derive(Clone)]
pub(crate) struct Step<'a> {
    pub kind: &'a str,
    pub enabled: bool,
    pub weight: f32,
    pub value: f32,
    pub settings: &'a Value,
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
    input: [i32; 4],
    depth: u16,
    cached: bool,
    steps: Vec<Step<'a>>,
}
impl<'a> Plan<'a> {
    pub(crate) fn new(
        frame: Frame,
        output: Region,
        depth: u16,
        steps: &[Step<'a>],
    ) -> Result<Self, String> {
        if ![8, 16].contains(&depth) || steps.len() > 64 {
            return Err("Invalid native mask depth or stage count".into());
        }
        let [w, h] = frame.dimensions();
        let b = output.bounds();
        let mut halo = 0i32;
        let mut cached = false;
        for step in steps {
            if !step.value.is_finite() {
                return Err("Mask value must be finite".into());
            }
            effects::validate_weight(step.weight)?;
            if ![
                "fill", "paint", "invert", "levels", "curves", "adjust", "blur", "gaussian",
            ]
            .contains(&step.kind)
            {
                return Err("Unsupported regional mask stage".into());
            }
            if ["curves", "adjust"].contains(&step.kind) {
                effects::normalized(step.kind, step.settings)?;
            }
            if step.kind == "gaussian" {
                effects::validate("blur", step.settings)?;
            }
            if !step.enabled || step.weight == 0. {
                continue;
            }
            cached |= ["curves", "adjust", "gaussian"].contains(&step.kind)
                || (step.kind == "blur" && step.value >= 0.5);
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
        let input = if cached {
            let c = [
                b[0].clamp(0, w as i32 - 1),
                b[1].clamp(0, h as i32 - 1),
                (b[2] - 1).clamp(0, w as i32 - 1) + 1,
                (b[3] - 1).clamp(0, h as i32 - 1) + 1,
            ];
            [
                (c[0] - halo).max(0),
                (c[1] - halo).max(0),
                (c[2] + halo).min(w as i32),
                (c[3] + halo).min(h as i32),
            ]
        } else {
            b
        };
        Ok(Self {
            frame,
            output,
            input,
            depth,
            cached,
            steps: steps.to_vec(),
        })
    }
    pub(crate) fn input_bounds(&self) -> [i32; 4] {
        self.input
    }
    pub(crate) fn paint_steps(&self) -> impl Iterator<Item = usize> + '_ {
        self.steps
            .iter()
            .enumerate()
            .filter(|(_, s)| s.enabled && s.weight > 0. && s.kind == "paint")
            .map(|(i, _)| i)
    }
    pub(crate) fn render(
        &self,
        budget: &ResidentBudget,
        check: &mut dyn FnMut() -> Result<(), String>,
        mut paint: impl FnMut(usize, i32, i32) -> Result<Pixel16, String>,
    ) -> Result<RegionalMask, String> {
        check()?;
        if !self.cached {
            return self.render_pointwise(budget, check, paint);
        }
        let (width, height) = (
            (self.input[2] - self.input[0]) as usize,
            (self.input[3] - self.input[1]) as usize,
        );
        let count = width
            .checked_mul(height)
            .ok_or("Mask region size overflow")?;
        let bytes = count
            .checked_mul(6)
            .and_then(|n| n.checked_add(131072))
            .ok_or("Mask scratch size overflow")?;
        let _charge = budget
            .reserve_bytes(bytes, bytes)
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
            check()?;
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
                        check()?;
                        for x in 0..width {
                            let p =
                                paint(index, x as i32 + self.input[0], y as i32 + self.input[1])?;
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
                        box_pass(&values, &mut scratch, width, height, radius, false);
                        box_pass(&scratch, &mut values, width, height, radius, true);
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
        for y in b[1]..b[3] {
            check()?;
            for x in b[0]..b[2] {
                let [fw, fh] = self.frame.dimensions();
                let sx = x.clamp(0, fw as i32 - 1);
                let sy = y.clamp(0, fh as i32 - 1);
                let v =
                    values[(sy - self.input[1]) as usize * width + (sx - self.input[0]) as usize];
                if depth == 16 {
                    output[at..at + 2].copy_from_slice(&v.to_be_bytes());
                    at += 2;
                } else {
                    output[at] = v as u8;
                    at += 1;
                }
            }
        }
        check()?;
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
        check: &mut dyn FnMut() -> Result<(), String>,
        mut paint: impl FnMut(usize, i32, i32) -> Result<Pixel16, String>,
    ) -> Result<RegionalMask, String> {
        let length = self.output.pixels() * if self.depth == 16 { 8 } else { 4 };
        let mut bytes = ScratchBytes::new(budget, length, length).map_err(|e| e.to_string())?;
        let mut at = 0;
        let b = self.output.bounds();
        for y in b[1]..b[3] {
            check()?;
            for x in b[0]..b[2] {
                if self.depth == 16 {
                    let mut value = 65535.0f64;
                    for (index, s) in self
                        .steps
                        .iter()
                        .enumerate()
                        .filter(|(_, s)| s.enabled && s.weight > 0.)
                    {
                        let before = value;
                        match s.kind {
                            "fill" => value = f64::from(s.value) * 257.,
                            "paint" => {
                                let p = paint(index, x, y)?;
                                let a = f64::from(p[3]) / 65535.;
                                value = value * (1. - a) + f64::from(p[0]) * a;
                            }
                            "invert" => value = 65535. - value,
                            "levels" => {
                                value = (value / 65535.).powf(f64::from(s.value.clamp(0.1, 5.)))
                                    * 65535.
                            }
                            _ => {}
                        }
                        value = before * (1. - f64::from(s.weight)) + value * f64::from(s.weight);
                    }
                    let value = value.clamp(0., 65535.) / 65535.;
                    if !value.is_finite() {
                        return Err("Pointwise native16 mask produced a non-finite factor".into());
                    }
                    bytes[at..at + 8].copy_from_slice(&value.to_bits().to_be_bytes());
                    at += 8;
                } else {
                    let mut value = 255.0f32;
                    for (index, s) in self
                        .steps
                        .iter()
                        .enumerate()
                        .filter(|(_, s)| s.enabled && s.weight > 0.)
                    {
                        let before = value;
                        match s.kind {
                            "fill" => value = s.value,
                            "paint" => {
                                let p = paint(index, x, y)?.map(project16);
                                let a = f32::from(p[3]) / 255.;
                                value = value * (1. - a) + f32::from(p[0]) * a;
                            }
                            "invert" => value = 255. - value,
                            "levels" => {
                                value = ((value / 255.).powf(s.value.clamp(0.1, 5.)) * 255.)
                                    .clamp(0., 255.)
                            }
                            _ => {}
                        }
                        value = before * (1. - s.weight) + value * s.weight;
                    }
                    let value = value.clamp(0., 255.) / 255.;
                    if !value.is_finite() {
                        return Err("Pointwise native8 mask produced a non-finite factor".into());
                    }
                    bytes[at..at + 4].copy_from_slice(&value.to_bits().to_be_bytes());
                    at += 4;
                }
            }
        }
        check()?;
        Ok(RegionalMask {
            bounds: b,
            depth: self.depth,
            fractional: true,
            bytes: bytes.freeze(),
        })
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
) {
    let (lines, length, stride) = if vertical {
        (width, height, width)
    } else {
        (height, width, 1)
    };
    let n = (radius * 2 + 1) as u64;
    for line in 0..lines {
        let base = if vertical { line } else { line * width };
        let at =
            |i: isize| u64::from(source[base + i.clamp(0, length as isize - 1) as usize * stride]);
        let mut sum: u64 = (-(radius as isize)..=radius as isize).map(at).sum();
        for i in 0..length {
            target[base + i * stride] = ((sum + n / 2) / n) as u16;
            sum = sum + at(i as isize + radius as isize + 1) - at(i as isize - radius as isize);
        }
    }
}
/// Prepare the raw layer-local mask. The enabled flag controls composition,
/// but must never erase disabled mask samples in a Photoshop channel.
pub(crate) fn prepare_scene(
    scene: &crate::engine::scene::SceneSnapshot,
    layer: crate::engine::native::LayerMetadata<'_>,
    bounds: [i32; 4],
    check: &mut dyn FnMut() -> Result<(), String>,
) -> Result<RegionalMask, String> {
    check()?;
    let [l, t, r, b] = bounds;
    let [ox, oy] = layer.origin();
    let local = |v: i32, origin| v.checked_sub(origin).ok_or("Native mask offset overflow");
    let output = Region::new([local(l, ox)?, local(t, oy)?, local(r, ox)?, local(b, oy)?])
        .map_err(|e| e.to_string())?;
    let mask = layer.mask().ok_or("Native layer has no mask channel")?;
    let count = mask.steps().len();
    if count > 64 {
        return Err("Too many regional mask stages".into());
    }
    let budget = scene.budget();
    let _metadata = budget
        .reserve_bytes(512 + count * 256, 32 * 1024)
        .map_err(|e| e.to_string())?;
    let steps: Vec<_> = mask
        .steps()
        .map(|s| Step {
            kind: s.kind(),
            enabled: s.enabled(),
            weight: s.weight(),
            value: s.value(),
            settings: s.settings(),
        })
        .collect();
    let frame = Frame::new(layer.pixels().width(), layer.pixels().height())?;
    let plan = Plan::new(frame, output, scene.metadata().depth(), &steps)?;
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
        let surface = scene.raster(step.pixels().slot())?;
        if !surface.uses_budget(budget) || !surface.uses_disk_budget(scene.disk_budget()) {
            return Err("Mask paint source differs from captured admission".into());
        }
        bases[i] = Some(pins.len());
        for y in (top..bottom).step_by(band_height) {
            let region = Region::new([left, y, right, (y + band_height as i32).min(bottom)])
                .map_err(|e| e.to_string())?;
            check()?;
            let prepared = surface.prepare_region(region, &mut || {
                check().map_err(|_| crate::large_document::Error::Cancelled)
            });
            check()?;
            pins.push(prepared.map_err(|e| e.to_string())?);
        }
    }

    plan.render(budget, check, |i, x, y| {
        let offset = usize::try_from(y - top)
            .map_err(|_| "Mask paint sample precedes footprint")?
            / band_height;
        let base = bases[i].ok_or("Mask paint stage was not prepared")?;
        if offset >= bands {
            return Err("Mask paint sample exceeds footprint".into());
        }
        pins[base + offset].pixel16(x, y).map_err(|e| e.to_string())
    })
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
                    .render(&budget, &mut || Ok(()), |i, x, y| {
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
            .render(&budget, &mut || Ok(()), |_, _, _| {
                reads += 1;
                Err("Synthetic native paint read failure".into())
            })
            .is_err());
        assert_eq!(reads, 1);
        assert_eq!(budget.used(), 0);
        assert!(plan
            .render(
                &budget,
                &mut || Err("Synthetic cancellation".into()),
                |_, _, _| panic!("Cancelled mask must not sample")
            )
            .is_err());
        assert_eq!(budget.used(), 0);
        assert!(plan
            .render(
                &ResidentBudget::new(1).unwrap(),
                &mut || Ok(()),
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
                let source = doc.layers[0].mask.as_ref().unwrap();
                let result = plan
                    .render(&budget, &mut || Ok(()), |i, x, y| {
                        Ok(source.steps[i].pixels.get16(x, y))
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
                .render(&budget, &mut || Ok(()), |i, x, y| {
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
