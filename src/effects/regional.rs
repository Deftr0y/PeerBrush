//! Explicit finite-support effect windows. Derived output retains its global
//! payload charge; no document source or full-frame cache is modified here.
use super::{styles, Effect, Image};
use crate::{
    depth16::Image16,
    large_document::{PinnedBytes, ResidentBudget, ScratchBytes},
    raster::{backed::Region, project16, Pixel, Pixel16},
};
use serde_json::Value;

#[derive(Clone, Copy, Debug)]
pub(crate) struct Frame([u32; 2]);
impl Frame {
    pub(crate) fn new(width: u32, height: u32) -> Result<Self, String> {
        if width == 0 || height == 0 || width > 16000 || height > 16000 {
            return Err("Effect frame exceeds the supported source geometry".into());
        }
        Ok(Self([width, height]))
    }
    pub(crate) fn dimensions(self) -> [u32; 2] {
        self.0
    }
}

pub(crate) struct Plan {
    frame: Frame,
    output: Region,
    input: [i32; 4],
    depth: u16,
    stages: Vec<(usize, Effect)>,
    stage_count: usize,
    bitmaps: Vec<Option<std::sync::Arc<super::patterns::PreparedBitmap>>>,
    resource_pool: Option<ResidentBudget>,
    _metadata_charge: Option<crate::large_document::Reservation>,
}

fn reach(kind: &str, settings: &Value) -> Result<i32, String> {
    let gaussian = |key, default| {
        super::gaussian_radii(super::number(settings, key, default))
            .iter()
            .sum::<usize>() as i32
    };
    if styles::contains(kind) {
        return Ok(if kind == "stroke" {
            settings["size"].as_f64().unwrap_or(0.).ceil() as i32 + 1
        } else {
            styles::smoothing_radii(kind, settings).iter().sum::<u32>() as i32
                + styles::field_padding(kind, settings)
        });
    }
    Ok(match kind {
        "blur" => gaussian("radius", 8.),
        "bloom" => gaussian("spread", 12.),
        "inner_glow" | "outer_glow" => 3 * (super::number(settings, "size", 0.) / 3.).ceil() as i32,
        "inner_shadow" | "drop_shadow" | "satin" => {
            3 * (super::number(settings, "size", 0.) / 3.).ceil() as i32
                + super::number(settings, "distance", 0.).ceil() as i32
                + 1
        }
        "bevel_emboss" => {
            3 * (super::number(settings, "size", 0.) / 3.).ceil() as i32
                + super::number(settings, "soften", 0.).ceil() as i32
                + 1
        }
        "stroke" => super::number(settings, "size", 0.).ceil() as i32 + 1,
        "levels" | "curves" | "adjust" | "color_balance" | "hsl" | "invert" | "grayscale"
        | "posterize" | "channel_clamp" | "contour" | "texture" | "color_overlay"
        | "gradient_overlay" | "pattern_overlay" => 0,
        "liquify" => {
            return Err("Regional Liquify requires an explicit inverse-mapped source plan".into())
        }
        _ => return Err("Effect has no verified regional support plan".into()),
    })
}

impl Plan {
    pub(crate) fn new(
        frame: Frame,
        output: Region,
        depth: u16,
        effects: &[Effect],
    ) -> Result<Self, String> {
        if ![8, 16].contains(&depth) || effects.len() > 32 {
            return Err("Invalid regional native depth or effect stack".into());
        }
        if effects
            .iter()
            .any(|e| super::patterns::active(&e.kind, &e.settings))
        {
            return Err(
                "Bitmap effects require shared-pool native preparation before regional planning"
                    .into(),
            );
        }
        Self::new_inner(
            frame,
            output,
            depth,
            effects,
            (0..effects.len()).map(|_| None).collect(),
            None,
            None,
        )
    }
    pub(crate) fn new_prepared(
        frame: Frame,
        output: Region,
        depth: u16,
        effects: &[Effect],
        pool: &ResidentBudget,
        bitmaps: &[Option<std::sync::Arc<super::patterns::PreparedBitmap>>],
    ) -> Result<Self, String> {
        if ![8, 16].contains(&depth) || effects.len() > 32 {
            return Err("Invalid regional native depth or effect stack".into());
        }
        if bitmaps.len() != effects.len() {
            return Err("Prepared bitmap owners do not match the captured stage roster".into());
        }
        for (effect, bitmap) in effects.iter().zip(bitmaps) {
            if super::patterns::active(&effect.kind, &effect.settings) != bitmap.is_some() {
                return Err(
                    "Regional bitmap styles require their exact admitted native payload".into(),
                );
            }
            if let Some(bitmap) = bitmap {
                if !bitmap.uses_budget(pool) {
                    return Err("Bitmap payload belongs to another resident pool".into());
                }
                if depth == 8 && bitmap.depth() == 16 {
                    return Err("A 16-bit bitmap pattern requires a 16-bit document".into());
                }
            }
        }
        let mut bytes = 65536usize;
        for effect in effects {
            metadata_bytes(&effect.settings, 0, &mut bytes)?;
        }
        let charge = pool
            .reserve_bytes(bytes, bytes)
            .map_err(|e| e.to_string())?;
        Self::new_inner(
            frame,
            output,
            depth,
            effects,
            bitmaps.to_vec(),
            Some(charge),
            Some(pool.clone()),
        )
    }
    fn new_inner(
        frame: Frame,
        output: Region,
        depth: u16,
        effects: &[Effect],
        bitmaps: Vec<Option<std::sync::Arc<super::patterns::PreparedBitmap>>>,
        metadata_charge: Option<crate::large_document::Reservation>,
        resource_pool: Option<ResidentBudget>,
    ) -> Result<Self, String> {
        if ![8, 16].contains(&depth) || effects.len() > 32 {
            return Err("Invalid regional native depth or effect stack".into());
        }
        let [width, height] = frame.0;
        let bounds = output.bounds();
        if bounds[0] < 0 || bounds[1] < 0 || bounds[2] > width as i32 || bounds[3] > height as i32 {
            return Err("Regional output is outside its effect frame".into());
        }
        let mut stages = Vec::new();
        stages
            .try_reserve_exact(effects.len())
            .map_err(|_| "Effect plan allocation failed")?;
        let mut halo = 0i32;
        for (index, effect) in effects.iter().enumerate() {
            super::validate_weight(effect.weight)?;
            if effect.kind == "liquify" {
                return Err(
                    "Regional Liquify requires typed inverse mapping and clip preparation".into(),
                );
            }
            let settings = if styles::contains(&effect.kind) {
                styles::normalized_prepared(
                    &effect.kind,
                    &effect.settings,
                    bitmaps[index].as_deref(),
                )?
            } else {
                super::normalized(&effect.kind, &effect.settings)?
            };
            if !effect.enabled || effect.weight == 0. {
                continue;
            }
            halo = halo
                .checked_add(reach(&effect.kind, &settings)?)
                .ok_or("Effect halo overflow")?;
            let mut normalized = effect.clone();
            normalized.settings = std::sync::Arc::new(settings);
            stages.push((index, normalized));
        }
        let input = [
            bounds[0].saturating_sub(halo).max(0),
            bounds[1].saturating_sub(halo).max(0),
            bounds[2].saturating_add(halo).min(width as i32),
            bounds[3].saturating_add(halo).min(height as i32),
        ];
        Ok(Self {
            frame,
            output,
            input,
            depth,
            stages,
            stage_count: effects.len(),
            bitmaps,
            resource_pool,
            _metadata_charge: metadata_charge,
        })
    }
    pub(crate) fn input_bounds(&self) -> [i32; 4] {
        self.input
    }
    pub(crate) fn output_bounds(&self) -> [i32; 4] {
        self.output.bounds()
    }

    /// Split the complete halo into admitted pin windows. The evaluator still
    /// receives one contiguous logical work image after all these pins succeed.
    pub(crate) fn input_regions(&self) -> Result<Vec<Region>, String> {
        let width = (self.input[2] - self.input[0]) as usize;
        let rows = (crate::raster::backed::MAX_REGION_PIXELS / width).max(1) as i32;
        let mut result = Vec::new();
        let count = ((self.input[3] - self.input[1]) as usize).div_ceil(rows as usize);
        result
            .try_reserve_exact(count)
            .map_err(|_| "Effect footprint allocation failed")?;
        let mut y = self.input[1];
        while y < self.input[3] {
            let bottom = y.saturating_add(rows).min(self.input[3]);
            result.push(
                Region::new([self.input[0], y, self.input[2], bottom])
                    .map_err(|e| e.to_string())?,
            );
            y = bottom;
        }
        Ok(result)
    }
    pub(crate) fn global_alpha_stages(&self) -> impl Iterator<Item = usize> + '_ {
        self.stages
            .iter()
            .filter(|(_, e)| {
                e.kind == "gradient_overlay"
                    && e.settings["pixel_grid"] == true
                    && e.settings["align_layer"] == true
            })
            .map(|(i, _)| *i)
    }
    pub(crate) fn global_field_stages(&self) -> impl Iterator<Item = usize> + '_ {
        self.stages
            .iter()
            .filter(|(_, e)| {
                styles::smoothing_radii(&e.kind, &e.settings)
                    .iter()
                    .any(|v| *v > 0)
            })
            .map(|(i, _)| *i)
    }
    pub(crate) fn field_request(
        &self,
        index: usize,
    ) -> Result<([u32; 2], [i32; 4], Vec<u32>), String> {
        let effect = &self
            .stages
            .iter()
            .find(|(i, _)| *i == index)
            .ok_or("Unknown regional effect stage")?
            .1;
        let pad = styles::field_padding(&effect.kind, &effect.settings);
        let b = self.input;
        Ok((
            self.frame.0,
            [
                (b[0] - pad).max(0),
                (b[1] - pad).max(0),
                (b[2] + pad).min(self.frame.0[0] as i32),
                (b[3] + pad).min(self.frame.0[1] as i32),
            ],
            styles::smoothing_radii(&effect.kind, &effect.settings),
        ))
    }

    /// Statistics belong to the exact input of each original effect index;
    /// callers must bind their reduction to the scene and stage identity.
    pub(crate) fn render(
        &self,
        budget: &ResidentBudget,
        statistics: &[Option<styles::AlphaBounds>],
        fields: &[Option<&super::field::PreparedField>],
        check: &mut dyn FnMut() -> Result<(), String>,
        mut sample: impl FnMut(i32, i32) -> Result<Pixel16, String>,
    ) -> Result<RegionalImage, String> {
        if self
            .resource_pool
            .as_ref()
            .is_some_and(|pool| !pool.same_pool(budget))
        {
            return Err("Regional execution belongs to another resident resource pool".into());
        }
        if fields
            .iter()
            .flatten()
            .any(|field| !field.uses_budget(budget))
        {
            return Err("Prepared style field belongs to another resident resource pool".into());
        }
        if statistics.len() != self.stage_count
            || self.global_alpha_stages().any(|i| statistics[i].is_none())
        {
            return Err("Regional effects require exact stage-global alpha statistics".into());
        }
        if fields.len() != self.stage_count
            || self.global_field_stages().any(|i| fields[i].is_none())
        {
            return Err(
                "Regional spatial styles require exact full-origin smoothing fields".into(),
            );
        }
        check()?;
        let (width, height) = (
            (self.input[2] - self.input[0]) as u32,
            (self.input[3] - self.input[1]) as u32,
        );
        let pixels = width as usize * height as usize;
        let stride = if self.depth == 16 { 8 } else { 4 };
        let mut scratch = 0u64;
        for (_, stage) in &self.stages {
            let work = if styles::contains(&stage.kind) {
                styles::working_bytes(width, height)
                    + 65536
                    + if stage.weight < 1. {
                        pixels as u64 * stride
                    } else {
                        0
                    }
                    + if self.depth == 8 {
                        pixels as u64 * 8
                    } else {
                        0
                    }
            } else if self.depth == 16 {
                crate::depth16::color::weighted_working_bytes(stage, width, height)?
            } else {
                super::weighted_working_bytes(stage, width, height)?
                    + if styles::contains(&stage.kind) {
                        pixels as u64 * 8
                    } else {
                        0
                    }
            };
            scratch = scratch.max(work);
        }
        let bytes = (pixels as u64)
            .checked_mul(stride)
            .and_then(|v| v.checked_add(scratch))
            .ok_or("Effect scratch length overflow")?;
        let bytes = usize::try_from(bytes).map_err(|_| "Effect scratch length overflow")?;
        // Keep this charge alive until every kernel-owned Vec has been dropped.
        // Each kernel allocation is separately fallible; outputs have their own
        // surviving charge rather than borrowing this temporary reservation.
        let _working = budget
            .reserve_bytes(bytes, bytes)
            .map_err(|e| e.to_string())?;
        if self.depth == 16 {
            let mut words = Vec::new();
            words
                .try_reserve_exact(pixels * 4)
                .map_err(|_| "Effect source allocation failed")?;
            for y in self.input[1]..self.input[3] {
                check()?;
                for x in self.input[0]..self.input[2] {
                    words.extend(sample(x, y)?);
                }
            }
            let mut image = Image16 {
                width,
                height,
                words,
            };
            for (index, effect) in &self.stages {
                check()?;
                let before = if effect.weight < 1. {
                    Some(copy(&image.words)?)
                } else {
                    None
                };
                if styles::contains(&effect.kind) {
                    styles::apply_window_prepared(
                        &mut image.words,
                        width,
                        height,
                        &effect.kind,
                        &effect.settings,
                        styles::Window {
                            origin: [self.input[0] as u32, self.input[1] as u32],
                            frame: self.frame.0,
                            alpha_bounds: statistics[*index],
                        },
                        fields[*index],
                        self.bitmaps[*index].as_deref(),
                    )?;
                } else {
                    crate::depth16::color::apply_cpu(&mut image, &effect.kind, &effect.settings)?;
                }
                if let Some(before) = before {
                    for (old, current) in
                        before.chunks_exact(4).zip(image.words.chunks_exact_mut(4))
                    {
                        current.copy_from_slice(&super::weighted_pixel(
                            old.try_into().unwrap(),
                            (&*current).try_into().unwrap(),
                            effect.weight,
                        ));
                    }
                }
            }
            self.finish(budget, check, |x, y| image.get(x, y))
        } else {
            let mut bytes = Vec::new();
            bytes
                .try_reserve_exact(pixels * 4)
                .map_err(|_| "Effect source allocation failed")?;
            for y in self.input[1]..self.input[3] {
                check()?;
                for x in self.input[0]..self.input[2] {
                    bytes.extend(sample(x, y)?.map(project16));
                }
            }
            let mut image = Image {
                width,
                height,
                bytes,
            };
            for (index, effect) in &self.stages {
                check()?;
                let before = if effect.weight < 1. {
                    Some(copy(&image.bytes)?)
                } else {
                    None
                };
                if styles::contains(&effect.kind) {
                    let mut words = Vec::new();
                    words
                        .try_reserve_exact(image.bytes.len())
                        .map_err(|_| "Style adapter allocation failed")?;
                    words.extend(image.bytes.iter().map(|v| u16::from(*v) * 257));
                    styles::apply_window_prepared(
                        &mut words,
                        width,
                        height,
                        &effect.kind,
                        &effect.settings,
                        styles::Window {
                            origin: [self.input[0] as u32, self.input[1] as u32],
                            frame: self.frame.0,
                            alpha_bounds: statistics[*index],
                        },
                        fields[*index],
                        self.bitmaps[*index].as_deref(),
                    )?;
                    for (out, word) in image.bytes.iter_mut().zip(words) {
                        *out = project16(word);
                    }
                } else {
                    super::apply_cpu(&mut image, &effect.kind, &effect.settings)?;
                }
                if let Some(before) = before {
                    for (old, current) in
                        before.chunks_exact(4).zip(image.bytes.chunks_exact_mut(4))
                    {
                        let p = super::weighted_pixel(
                            old.try_into().map(|p: [u8; 4]| p.map(u16::from)).unwrap(),
                            (&*current)
                                .try_into()
                                .map(|p: [u8; 4]| p.map(u16::from))
                                .unwrap(),
                            effect.weight,
                        );
                        current.copy_from_slice(&p.map(|v| v as u8));
                    }
                }
            }
            self.finish(budget, check, |x, y| {
                image.get(x, y).map(|v| u16::from(v) * 257)
            })
        }
    }
    fn finish(
        &self,
        budget: &ResidentBudget,
        check: &mut dyn FnMut() -> Result<(), String>,
        mut sample: impl FnMut(i32, i32) -> Pixel16,
    ) -> Result<RegionalImage, String> {
        let length = self.output.pixels() * if self.depth == 16 { 8 } else { 4 };
        let mut bytes = ScratchBytes::new(budget, length, length).map_err(|e| e.to_string())?;
        let mut at = 0;
        let bounds = self.output.bounds();
        for y in bounds[1]..bounds[3] {
            check()?;
            for x in bounds[0]..bounds[2] {
                let pixel = sample(x - self.input[0], y - self.input[1]);
                if self.depth == 16 {
                    for v in pixel {
                        bytes[at..at + 2].copy_from_slice(&v.to_be_bytes());
                        at += 2;
                    }
                } else {
                    bytes[at..at + 4].copy_from_slice(&pixel.map(project16));
                    at += 4;
                }
            }
        }
        check()?;
        Ok(RegionalImage {
            bounds: self.output.bounds(),
            depth: self.depth,
            bytes: bytes.freeze(),
        })
    }
}

fn copy<T: Copy>(source: &[T]) -> Result<Vec<T>, String> {
    let mut result = Vec::new();
    result
        .try_reserve_exact(source.len())
        .map_err(|_| "Effect weight allocation failed")?;
    result.extend_from_slice(source);
    Ok(result)
}

// Conservative node/key/string admission for captured normalized settings.
// A BTreeMap node can keep spare slots; charge a full KiB per value/member
// rather than claiming serialized JSON length measures its allocation.
fn metadata_bytes(value: &Value, depth: u32, bytes: &mut usize) -> Result<(), String> {
    if depth > 64 {
        return Err("Regional settings nesting exceeds metadata admission".into());
    }
    *bytes = bytes
        .checked_add(1024)
        .ok_or("Regional settings length overflow")?;
    match value {
        Value::String(s) => {
            *bytes = bytes
                .checked_add(s.len())
                .ok_or("Regional settings length overflow")?
        }
        Value::Array(values) => {
            for v in values {
                metadata_bytes(v, depth + 1, bytes)?;
            }
        }
        Value::Object(values) => {
            for (k, v) in values {
                *bytes = bytes
                    .checked_add(k.len() + 1024)
                    .ok_or("Regional settings length overflow")?;
                metadata_bytes(v, depth + 1, bytes)?;
            }
        }
        _ => {}
    }
    if *bytes > 64 * 1024 * 1024 {
        return Err(
            "Regional normalized metadata exceeds its bounded shared-pool admission".into(),
        );
    }
    Ok(())
}

#[derive(Clone)]
pub(crate) struct RegionalImage {
    bounds: [i32; 4],
    depth: u16,
    bytes: PinnedBytes,
}
impl RegionalImage {
    pub(crate) fn from_native(
        bounds: Region,
        depth: u16,
        bytes: PinnedBytes,
    ) -> Result<Self, String> {
        let stride = match depth {
            8 => 4,
            16 => 8,
            _ => return Err("Derived native image requires depth8or16".into()),
        };
        if bytes.len() != bounds.pixels() * stride {
            return Err("Derived native image bytes differ from their declared footprint".into());
        }
        Ok(Self {
            bounds: bounds.bounds(),
            depth,
            bytes,
        })
    }
    pub(crate) fn uses_budget(&self, budget: &ResidentBudget) -> bool {
        self.bytes.uses_budget(budget)
    }
    pub(crate) fn bounds(&self) -> [i32; 4] {
        self.bounds
    }
    pub(crate) fn depth(&self) -> u16 {
        self.depth
    }
    pub(crate) fn bytes(&self) -> &[u8] {
        &self.bytes
    }
    pub(crate) fn pixel16(&self, x: i32, y: i32) -> Result<Pixel16, String> {
        if x < self.bounds[0] || y < self.bounds[1] || x >= self.bounds[2] || y >= self.bounds[3] {
            return Err("Derived sample is outside its prepared effect output".into());
        }
        let stride = if self.depth == 16 { 8 } else { 4 };
        let at = ((y - self.bounds[1]) as usize * (self.bounds[2] - self.bounds[0]) as usize
            + (x - self.bounds[0]) as usize)
            * stride;
        Ok(std::array::from_fn(|c| {
            if self.depth == 16 {
                u16::from_be_bytes([self.bytes[at + c * 2], self.bytes[at + c * 2 + 1]])
            } else {
                u16::from(self.bytes[at + c]) * 257
            }
        }))
    }
    pub(crate) fn pixel(&self, x: i32, y: i32) -> Result<Pixel, String> {
        Ok(self.pixel16(x, y)?.map(project16))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn admitted_bitmap_payloads_match_native_oracles_and_survive_plan_cache_drop() {
        use base64::{engine::general_purpose::STANDARD, Engine as _};
        let area = Region::new([19, 21, 39, 43]).unwrap();
        for depth in [8, 16] {
            for kind in ["pattern_overlay", "texture"] {
                let pool = ResidentBudget::new(16 * 1024 * 1024).unwrap();
                let words = [
                    12347u16, 33559, 51237, 65535, 54321, 12345, 45679, 27139, 19001, 27003, 37007,
                    49151, 61001, 51003, 41009, 65535,
                ]
                .map(|v| {
                    if depth == 8 {
                        u16::from(project16(v)) * 257
                    } else {
                        v
                    }
                });
                let png = if depth == 16 {
                    crate::raster::png16(2, 2, &words).unwrap()
                } else {
                    crate::raster::png(2, 2, &words.map(project16)).unwrap()
                };
                let source = STANDARD.encode(png);
                let mut payload = ScratchBytes::new(&pool, 32, 32).unwrap();
                for (out, value) in payload.chunks_exact_mut(2).zip(words) {
                    out.copy_from_slice(&value.to_be_bytes());
                }
                let bitmap = std::sync::Arc::new(
                    super::super::patterns::PreparedBitmap::from_native(
                        &source,
                        2,
                        2,
                        depth,
                        payload.freeze(),
                        &pool,
                        &mut || Ok(()),
                    )
                    .unwrap(),
                );
                let mut effect = effect(kind);
                let settings = std::sync::Arc::make_mut(&mut effect.settings);
                settings["pattern"] = json!("bitmap");
                settings["bitmap_png"] = json!(source);
                settings["bitmap_scale"] = json!(1.7);
                settings["bitmap_angle"] = json!(23.);
                settings["phase_x"] = json!(-1.3);
                settings["phase_y"] = json!(2.7);
                settings["sampling"] = json!("bilinear");
                effect.weight = 0.73;
                assert!(
                    Plan::new(Frame::new(61, 67).unwrap(), area, depth, &[effect.clone()]).is_err()
                );
                let plan = Plan::new_prepared(
                    Frame::new(61, 67).unwrap(),
                    area,
                    depth,
                    &[effect.clone()],
                    &pool,
                    &[Some(bitmap.clone())],
                )
                .unwrap();
                let other_pool = ResidentBudget::new(pool.limit()).unwrap();
                assert!(plan
                    .render(
                        &other_pool,
                        &[None],
                        &[None],
                        &mut || Ok(()),
                        |_, _| panic!("Wrong-pool execution must refuse before source access")
                    )
                    .is_err());
                let actual = plan
                    .render(&pool, &[None], &[None], &mut || Ok(()), |x, y| {
                        Ok(pixel(x, y, depth))
                    })
                    .unwrap();
                let mut reference = Image16 {
                    width: 61,
                    height: 67,
                    words: (0..67)
                        .flat_map(|y| (0..61).flat_map(move |x| pixel(x, y, depth)))
                        .collect(),
                };
                if depth == 16 {
                    let before = reference.words.clone();
                    peerbrush::depth16::color::apply_cpu(&mut reference, kind, &effect.settings)
                        .unwrap();
                    for (old, out) in before
                        .chunks_exact(4)
                        .zip(reference.words.chunks_exact_mut(4))
                    {
                        out.copy_from_slice(&super::super::weighted_pixel(
                            old.try_into().unwrap(),
                            (&*out).try_into().unwrap(),
                            effect.weight,
                        ));
                    }
                } else {
                    let mut image = Image {
                        width: 61,
                        height: 67,
                        bytes: reference.words.iter().map(|v| project16(*v)).collect(),
                    };
                    let before = image.bytes.clone();
                    peerbrush::effects::apply_cpu(&mut image, kind, &effect.settings).unwrap();
                    for (old, out) in before.chunks_exact(4).zip(image.bytes.chunks_exact_mut(4)) {
                        let p = super::super::weighted_pixel(
                            old.try_into().map(|p: [u8; 4]| p.map(u16::from)).unwrap(),
                            (&*out)
                                .try_into()
                                .map(|p: [u8; 4]| p.map(u16::from))
                                .unwrap(),
                            effect.weight,
                        );
                        out.copy_from_slice(&p.map(|v| v as u8));
                    }
                    reference.words = image
                        .bytes
                        .into_iter()
                        .map(|v| u16::from(v) * 257)
                        .collect();
                }
                for y in 21..43 {
                    for x in 19..39 {
                        assert_eq!(
                            actual.pixel16(x, y).unwrap(),
                            reference.get(x, y),
                            "{kind} native{depth} ({x},{y})"
                        );
                    }
                }
                drop(actual);
                drop(plan);
                assert_eq!(pool.used(), 32);
                let mut changed = effect.clone();
                std::sync::Arc::make_mut(&mut changed.settings)["bitmap_png"] =
                    json!("different-source");
                assert!(Plan::new_prepared(
                    Frame::new(61, 67).unwrap(),
                    area,
                    depth,
                    &[changed],
                    &pool,
                    &[Some(bitmap.clone())]
                )
                .is_err());
                let foreign = ResidentBudget::new(pool.limit()).unwrap();
                assert!(Plan::new_prepared(
                    Frame::new(61, 67).unwrap(),
                    area,
                    depth,
                    &[effect],
                    &foreign,
                    &[Some(bitmap.clone())]
                )
                .is_err());
                drop(bitmap);
                assert_eq!(pool.used(), 0);
                assert_eq!(foreign.used(), 0);
            }
        }
    }
    fn effect(kind: &str) -> Effect {
        Effect {
            id: "regional-test".into(),
            kind: kind.into(),
            enabled: true,
            weight: 1.,
            settings: std::sync::Arc::new(super::super::defaults(kind)),
        }
    }
    fn pixel(x: i32, y: i32, depth: u16) -> Pixel16 {
        let p = [
            10001 + (x as u32 * 317 + y as u32 * 127) % 55001,
            21113 + (x as u32 * 137) % 40003,
            32213 + (y as u32 * 211) % 30011,
            if (x + y) % 7 == 0 {
                0
            } else {
                21001 + (x as u32 * 173 + y as u32 * 379) % 44535
            },
        ];
        p.map(|v| {
            if depth == 8 {
                (v / 257 * 257) as u16
            } else {
                v as u16
            }
        })
    }
    fn source_region(
        region: Region,
        depth: u16,
        budget: &ResidentBudget,
    ) -> Result<RegionalImage, String> {
        let length = region.pixels() * if depth == 16 { 8 } else { 4 };
        let mut bytes = ScratchBytes::new(budget, length, length).map_err(|e| e.to_string())?;
        let mut at = 0;
        let [left, top, right, bottom] = region.bounds();
        for y in top..bottom {
            for x in left..right {
                let p = pixel(x, y, depth);
                if depth == 16 {
                    for v in p {
                        bytes[at..at + 2].copy_from_slice(&v.to_be_bytes());
                        at += 2;
                    }
                } else {
                    bytes[at..at + 4].copy_from_slice(&p.map(project16));
                    at += 4;
                }
            }
        }
        RegionalImage::from_native(region, depth, bytes.freeze())
    }
    #[test]
    fn exact_native_regions_match_whole_frame_effects_and_weighted_stacks() {
        let (w, h) = (97, 101);
        let area = Region::new([39, 43, 62, 71]).unwrap();
        let kinds: Vec<_> = super::super::KINDS
            .iter()
            .copied()
            .filter(|v| *v != "liquify")
            .collect();
        for depth in [8, 16] {
            for kind in &kinds {
                let stages = vec![effect(kind)];
                let plan = Plan::new(Frame::new(w, h).unwrap(), area, depth, &stages).unwrap();
                let budget = ResidentBudget::new(32 * 1024 * 1024).unwrap();
                let mut fields: Vec<_> = (0..stages.len()).map(|_| None).collect();
                for index in plan.global_field_stages() {
                    let (frame, bounds, radii) = plan.field_request(index).unwrap();
                    fields[index] = Some(
                        super::super::field::prepare(
                            frame,
                            bounds,
                            &radii,
                            depth,
                            &budget,
                            &mut || Ok(()),
                            |r| source_region(r, depth, &budget),
                        )
                        .unwrap(),
                    );
                }
                let field_refs: Vec<_> = fields.iter().map(Option::as_ref).collect();
                let output = plan
                    .render(&budget, &[None], &field_refs, &mut || Ok(()), |x, y| {
                        Ok(pixel(x, y, depth))
                    })
                    .unwrap();
                drop(field_refs);
                drop(fields);
                if depth == 16 {
                    let mut image = Image16 {
                        width: w,
                        height: h,
                        words: (0..h)
                            .flat_map(|y| {
                                (0..w).flat_map(move |x| pixel(x as i32, y as i32, depth))
                            })
                            .collect(),
                    };
                    crate::depth16::color::apply_cpu(&mut image, kind, &stages[0].settings)
                        .unwrap();
                    for y in 43..71 {
                        for x in 39..62 {
                            assert_eq!(
                                output.pixel16(x, y).unwrap(),
                                image.get(x, y),
                                "{kind} {depth} ({x},{y})"
                            );
                        }
                    }
                } else {
                    let mut image = Image {
                        width: w,
                        height: h,
                        bytes: (0..h)
                            .flat_map(|y| {
                                (0..w).flat_map(move |x| {
                                    pixel(x as i32, y as i32, depth).map(project16)
                                })
                            })
                            .collect(),
                    };
                    super::super::apply_cpu(&mut image, kind, &stages[0].settings).unwrap();
                    for y in 43..71 {
                        for x in 39..62 {
                            assert_eq!(
                                output.pixel(x, y).unwrap(),
                                image.get(x, y),
                                "{kind} {depth} ({x},{y})"
                            );
                        }
                    }
                }
                assert_eq!(
                    budget.used(),
                    area.pixels() * if depth == 16 { 8 } else { 4 }
                );
                drop(output);
                assert_eq!(budget.used(), 0);
            }
        }
    }
    #[test]
    fn missing_statistics_budget_and_sample_faults_return_no_partial_image() {
        let budget = ResidentBudget::new(1024 * 1024).unwrap();
        let mut gradient = effect("gradient_overlay");
        let settings = std::sync::Arc::make_mut(&mut gradient.settings);
        settings["pixel_grid"] = json!(true);
        settings["align_layer"] = json!(true);
        let plan = Plan::new(
            Frame::new(100, 100).unwrap(),
            Region::new([10, 11, 30, 31]).unwrap(),
            16,
            &[gradient],
        )
        .unwrap();
        let mut calls = 0;
        assert!(plan
            .render(&budget, &[None], &[None], &mut || Ok(()), |_, _| {
                calls += 1;
                Ok([0; 4])
            })
            .is_err());
        assert_eq!(calls, 0);
        assert_eq!(budget.used(), 0);
        let plan = Plan::new(
            Frame::new(100, 100).unwrap(),
            Region::new([10, 11, 30, 31]).unwrap(),
            16,
            &[effect("invert")],
        )
        .unwrap();
        let too_small = ResidentBudget::new(1).unwrap();
        assert!(plan
            .render(&too_small, &[None], &[None], &mut || Ok(()), |_, _| {
                calls += 1;
                Ok([0; 4])
            })
            .is_err());
        assert_eq!(calls, 0);
        assert!(plan
            .render(&budget, &[None], &[None], &mut || Ok(()), |_, _| {
                calls += 1;
                if calls == 7 {
                    Err("synthetic native source read failure".into())
                } else {
                    Ok([12347; 4])
                }
            })
            .is_err());
        assert_eq!(budget.used(), 0);
    }
    #[test]
    fn prepared_field_charge_must_share_the_actual_execution_pool() {
        let plan = Plan::new(
            Frame::new(97, 101).unwrap(),
            Region::new([39, 43, 62, 71]).unwrap(),
            16,
            &[effect("outer_glow")],
        )
        .unwrap();
        let pool = ResidentBudget::new(4 * 1024 * 1024).unwrap();
        let foreign = ResidentBudget::new(pool.limit()).unwrap();
        let (frame, bounds, radii) = plan.field_request(0).unwrap();
        let field = super::super::field::prepare(
            frame,
            bounds,
            &radii,
            16,
            &foreign,
            &mut || Ok(()),
            |r| source_region(r, 16, &foreign),
        )
        .unwrap();
        assert!(field.uses_budget(&foreign));
        assert!(!field.uses_budget(&pool));
        assert!(plan
            .render(
                &pool,
                &[None],
                &[Some(&field)],
                &mut || Ok(()),
                |_, _| panic!("A foreign field must refuse before reading native pixels")
            )
            .is_err());
        drop(field);
        assert_eq!(pool.used(), 0);
        assert_eq!(foreign.used(), 0);
    }
    #[test]
    fn weighted_native_stacks_match_exact_full_frame_composition() {
        let (w, h) = (71, 73);
        let area = Region::new([29, 31, 48, 51]).unwrap();
        let mut stages = vec![effect("invert"), effect("blur"), effect("hsl")];
        stages[0].weight = 0.37;
        stages[1].weight = 0.61;
        stages[2].weight = 0.23;
        std::sync::Arc::make_mut(&mut stages[1].settings)["radius"] = json!(1.3);
        std::sync::Arc::make_mut(&mut stages[2].settings)["hue"] = json!(37.);
        for depth in [8, 16] {
            let plan = Plan::new(Frame::new(w, h).unwrap(), area, depth, &stages).unwrap();
            let budget = ResidentBudget::new(16 * 1024 * 1024).unwrap();
            let output = plan
                .render(
                    &budget,
                    &[None, None, None],
                    &[None, None, None],
                    &mut || Ok(()),
                    |x, y| Ok(pixel(x, y, depth)),
                )
                .unwrap();
            if depth == 16 {
                let mut image = Image16 {
                    width: w,
                    height: h,
                    words: (0..h)
                        .flat_map(|y| (0..w).flat_map(move |x| pixel(x as i32, y as i32, depth)))
                        .collect(),
                };
                for stage in &stages {
                    let before = image.words.clone();
                    crate::depth16::color::apply_cpu(&mut image, &stage.kind, &stage.settings)
                        .unwrap();
                    for (old, current) in
                        before.chunks_exact(4).zip(image.words.chunks_exact_mut(4))
                    {
                        current.copy_from_slice(&super::super::weighted_pixel(
                            old.try_into().unwrap(),
                            (&*current).try_into().unwrap(),
                            stage.weight,
                        ));
                    }
                }
                for y in 31..51 {
                    for x in 29..48 {
                        assert_eq!(output.pixel16(x, y).unwrap(), image.get(x, y));
                    }
                }
            } else {
                let mut image = Image {
                    width: w,
                    height: h,
                    bytes: (0..h)
                        .flat_map(|y| {
                            (0..w)
                                .flat_map(move |x| pixel(x as i32, y as i32, depth).map(project16))
                        })
                        .collect(),
                };
                for stage in &stages {
                    let before = image.bytes.clone();
                    super::super::apply_cpu(&mut image, &stage.kind, &stage.settings).unwrap();
                    for (old, current) in
                        before.chunks_exact(4).zip(image.bytes.chunks_exact_mut(4))
                    {
                        let p = super::super::weighted_pixel(
                            old.try_into().map(|p: [u8; 4]| p.map(u16::from)).unwrap(),
                            (&*current)
                                .try_into()
                                .map(|p: [u8; 4]| p.map(u16::from))
                                .unwrap(),
                            stage.weight,
                        );
                        current.copy_from_slice(&p.map(|v| v as u8));
                    }
                }
                for y in 31..51 {
                    for x in 29..48 {
                        assert_eq!(output.pixel(x, y).unwrap(), image.get(x, y));
                    }
                }
            }
        }
    }
    #[test]
    fn large_frame_requests_pin_bounded_full_halos_and_outputs_keep_their_charge() {
        let budget = ResidentBudget::new(16 * 1024 * 1024).unwrap();
        let area = Region::new([7900, 7900, 8156, 8156]).unwrap();
        let plan = Plan::new(
            Frame::new(16000, 16000).unwrap(),
            area,
            16,
            &[effect("blur")],
        )
        .unwrap();
        let regions = plan.input_regions().unwrap();
        let input = plan.input_bounds();
        assert_eq!(
            regions.iter().map(|v| v.pixels()).sum::<usize>(),
            (input[2] - input[0]) as usize * (input[3] - input[1]) as usize
        );
        assert!(regions
            .iter()
            .all(|r| r.pixels() <= crate::raster::backed::MAX_REGION_PIXELS));
        let image = plan
            .render(&budget, &[None], &[None], &mut || Ok(()), |_, _| {
                Ok([12347, 33559, 51237, 65535])
            })
            .unwrap();
        assert_eq!(
            image.pixel16(8000, 8000).unwrap(),
            [12347, 33559, 51237, 65535]
        );
        assert!(image.pixel16(7899, 8000).is_err());
        let clone = image.clone();
        drop(image);
        assert_eq!(budget.used(), area.pixels() * 8);
        drop(clone);
        assert_eq!(budget.used(), 0);
        // This is one regional component, not full16000 editor qualification.
    }
}
