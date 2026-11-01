//! Runtime headers share physical payload owners; wire DTOs remain independent.
use super::{
    payload::{Footprint, Measure},
    *,
};

fn text(value: &str) -> Result<String> {
    let mut out = String::new();
    out.try_reserve_exact(value.len())
        .map_err(|_| "Native metadata text allocation refused")?;
    out.push_str(value);
    Ok(out)
}
fn option_text(value: &Option<String>) -> Result<Option<String>> {
    value.as_deref().map(text).transpose()
}
fn vector<T, U>(
    values: &[T],
    check: &dyn Fn() -> Result<()>,
    mut copy: impl FnMut(&T) -> Result<U>,
) -> Result<Vec<U>> {
    let mut out = Vec::new();
    out.try_reserve_exact(values.len())
        .map_err(|_| "Native metadata header allocation refused")?;
    if out.capacity() > values.len() {
        return Err("Native metadata header exceeds admission".into());
    }
    for value in values {
        check()?;
        out.push(copy(value)?);
    }
    Ok(out)
}
fn owned_vector<T>(
    values: Vec<T>,
    check: &dyn Fn() -> Result<()>,
    mut bind: impl FnMut(T) -> Result<T>,
) -> Result<Vec<T>> {
    let mut out = Vec::new();
    out.try_reserve_exact(values.len())
        .map_err(|_| "Native metadata header allocation refused")?;
    if out.capacity() > values.len() {
        return Err("Native metadata header exceeds admission".into());
    }
    for value in values {
        check()?;
        out.push(bind(value)?);
    }
    Ok(out)
}
fn raster(value: &RasterRef36, check: &dyn Fn() -> Result<()>) -> Result<RasterRef36> {
    check()?;
    Ok(RasterRef36 {
        id: value.id,
        width: value.width,
        height: value.height,
        depth: value.depth,
        retained: value
            .retained
            .as_ref()
            .map(|original| -> Result<_> {
                if original.pixels.retained.is_some() {
                    return Err("Nested retained native metadata".into());
                }
                Ok(Box::new(Retained36 {
                    pixels: raster(&original.pixels, check)?,
                    matrix: original.matrix,
                    projective: original.projective,
                    warps: vector(&original.warps, check, |warp| Ok(warp.clone()))?,
                }))
            })
            .transpose()?,
    })
}
fn coverage(value: &Coverage36, check: &dyn Fn() -> Result<()>) -> Result<Coverage36> {
    Ok(Coverage36 {
        mask: raster(&value.mask, check)?,
        bounds: value.bounds,
        origin: value.origin,
        contours: value.contours.shared()?,
    })
}
fn effect(value: &Effect36, check: &dyn Fn() -> Result<()>) -> Result<Effect36> {
    Ok(Effect36 {
        id: text(&value.id)?,
        kind: text(&value.kind)?,
        enabled: value.enabled,
        weight: value.weight,
        settings: value.settings.shared()?,
        clips: vector(&value.clips, check, |clip| {
            Ok(EffectClip36 {
                stroke: clip.stroke,
                coverage: coverage(&clip.coverage, check)?,
            })
        })?,
    })
}
fn layer(value: &Layer36, check: &dyn Fn() -> Result<()>) -> Result<Layer36> {
    Ok(Layer36 {
        id: text(&value.id)?,
        name: text(&value.name)?,
        kind: text(&value.kind)?,
        parent: option_text(&value.parent)?,
        clip_to: option_text(&value.clip_to)?,
        visible: value.visible,
        locked: value.locked,
        opacity: value.opacity,
        blend: text(&value.blend)?,
        x: value.x,
        y: value.y,
        pixels: raster(&value.pixels, check)?,
        color: value.color,
        mask: value
            .mask
            .as_ref()
            .map(|mask| -> Result<_> {
                Ok(Mask36 {
                    enabled: mask.enabled,
                    linked: mask.linked,
                    steps: vector(&mask.steps, check, |step| {
                        Ok(MaskStep36 {
                            id: text(&step.id)?,
                            kind: text(&step.kind)?,
                            enabled: step.enabled,
                            weight: step.weight,
                            value: step.value,
                            pixels: raster(&step.pixels, check)?,
                            settings: step.settings.shared()?,
                        })
                    })?,
                })
            })
            .transpose()?,
        effects: vector(&value.effects, check, |value| effect(value, check))?,
        psd_metadata: value.psd_metadata.shared()?,
        source: value
            .source
            .as_ref()
            .map(|source| -> Result<_> {
                Ok(Source36 {
                    width: source.width,
                    height: source.height,
                    matrix: source.matrix,
                    content: source.content.shared()?,
                })
            })
            .transpose()?,
    })
}
impl Source36 {
    pub(crate) fn copy_runtime(
        &self,
        pool: &ResidentBudget,
        check: &dyn Fn() -> Result<()>,
    ) -> Result<Self> {
        let content = if self.content.uses_budget(pool) {
            self.content.shared()?
        } else {
            MetadataPayload::copy_from(self.content.value(), pool, check)?
        };
        Ok(Self {
            width: self.width,
            height: self.height,
            matrix: self.matrix,
            content,
        })
    }
}
impl Document36 {
    pub(crate) fn copy_runtime(
        &self,
        pool: &ResidentBudget,
        check: &dyn Fn() -> Result<()>,
    ) -> Result<Admitted<Self>> {
        if !self.runtime_bound(pool) {
            return Err("Only owned native metadata can share runtime payloads".into());
        }
        self.visit_rasters(|_| Ok(()))?;
        let bytes = self.header_bytes(check)?;
        let owner = MetadataCharge::reserve(pool, bytes, check)?;
        let copied = Self {
            id: text(&self.id)?,
            name: text(&self.name)?,
            width: self.width,
            height: self.height,
            revision: self.revision,
            bit_depth: self.bit_depth,
            icc_profile: self
                .icc_profile
                .as_ref()
                .map(MetadataPayload::shared)
                .transpose()?,
            srgb_tagged: self.srgb_tagged,
            psd_global_mask: self
                .psd_global_mask
                .as_ref()
                .map(MetadataPayload::shared)
                .transpose()?,
            layers: vector(&self.layers, check, |value| layer(value, check))?,
            filters: vector(&self.filters, check, |value| effect(value, check))?,
            selection: self.selection,
            selection_polygon: self
                .selection_polygon
                .as_ref()
                .map(MetadataPayload::shared)
                .transpose()?,
            selection_coverage: self
                .selection_coverage
                .as_ref()
                .map(|v| coverage(v, check))
                .transpose()?,
            selection_previous: self
                .selection_previous
                .as_ref()
                .map(|v| coverage(v, check))
                .transpose()?,
            read_only: self.read_only,
            warnings: self.warnings.shared()?,
        };
        if !copied.runtime_bound(pool) {
            return Err("Native metadata payload admission differs".into());
        }
        check()?;
        Ok(Admitted::from_parts(Arc::new(copied), owner))
    }
    pub(crate) fn bind_runtime(
        self,
        pool: &ResidentBudget,
        check: &dyn Fn() -> Result<()>,
    ) -> Result<(Self, Arc<MetadataCharge>)> {
        if self
            .icc_profile
            .as_ref()
            .is_some_and(|v| v.len() > super::icc::MAX_BYTES)
            || self
                .selection_polygon
                .as_ref()
                .is_some_and(|v| v.len() > 500_000)
        {
            return Err("Native profile or polygon exceeds its field admission".into());
        }
        self.visit_rasters(|_| Ok(()))?;
        // This covers the NEW header arrays before they are allocated. The old
        // parse/header owner remains held by Admitted::rehome_unique throughout.
        let owner = MetadataCharge::reserve(pool, self.header_bytes(check)?, check)?;
        let Self {
            id,
            name,
            width,
            height,
            revision,
            bit_depth,
            icc_profile,
            srgb_tagged,
            psd_global_mask,
            layers,
            filters,
            selection,
            selection_polygon,
            selection_coverage,
            selection_previous,
            read_only,
            warnings,
        } = self;
        let metadata = Self {
            id,
            name,
            width,
            height,
            revision,
            bit_depth,
            icc_profile: icc_profile.map(|v| v.bind(pool, check)).transpose()?,
            srgb_tagged,
            psd_global_mask: psd_global_mask.map(|v| v.bind(pool, check)).transpose()?,
            layers: owned_vector(layers, check, |v| bind_layer(v, pool, check))?,
            filters: owned_vector(filters, check, |v| bind_effect(v, pool, check))?,
            selection,
            selection_polygon: selection_polygon.map(|v| v.bind(pool, check)).transpose()?,
            selection_coverage: selection_coverage
                .map(|v| bind_coverage(v, pool, check))
                .transpose()?,
            selection_previous: selection_previous
                .map(|v| bind_coverage(v, pool, check))
                .transpose()?,
            read_only,
            warnings: warnings.bind(pool, check)?,
        };
        if !metadata.runtime_bound(pool) {
            return Err("Native metadata payload binding is incomplete".into());
        }
        check()?;
        Ok((metadata, owner))
    }
    pub(crate) fn runtime_bound(&self, pool: &ResidentBudget) -> bool {
        let coverage = |v: &Coverage36| v.contours.uses_budget(pool);
        let effect = |v: &Effect36| {
            v.settings.uses_budget(pool) && v.clips.iter().all(|v| coverage(&v.coverage))
        };
        self.icc_profile
            .as_ref()
            .is_none_or(|v| v.uses_budget(pool))
            && self
                .psd_global_mask
                .as_ref()
                .is_none_or(|v| v.uses_budget(pool))
            && self.warnings.uses_budget(pool)
            && self
                .selection_polygon
                .as_ref()
                .is_none_or(|v| v.uses_budget(pool))
            && self.selection_coverage.as_ref().is_none_or(coverage)
            && self.selection_previous.as_ref().is_none_or(coverage)
            && self.layers.iter().all(|l| {
                l.psd_metadata.uses_budget(pool)
                    && l.source
                        .as_ref()
                        .is_none_or(|s| s.content.uses_budget(pool))
                    && l.effects.iter().all(effect)
                    && l.mask
                        .as_ref()
                        .is_none_or(|m| m.steps.iter().all(|s| s.settings.uses_budget(pool)))
            })
            && self.filters.iter().all(effect)
    }
    fn header_bytes(&self, check: &dyn Fn() -> Result<()>) -> Result<usize> {
        let mut measure = Measure::new(check);
        measure.add(std::mem::size_of::<Self>() + 512)?;
        self.id.heap(&mut measure)?;
        self.name.heap(&mut measure)?;
        measure.vector(&self.layers)?;
        for layer in &self.layers {
            if !layer.opacity.is_finite()
                || layer
                    .source
                    .as_ref()
                    .is_some_and(|s| s.matrix.iter().any(|v| !v.is_finite()))
            {
                return Err("Nonfinite native header metadata".into());
            }
            for value in [&layer.id, &layer.name, &layer.kind, &layer.blend] {
                value.heap(&mut measure)?;
            }
            for value in [layer.parent.as_ref(), layer.clip_to.as_ref()]
                .into_iter()
                .flatten()
            {
                value.heap(&mut measure)?;
            }
            raster_heap(&layer.pixels, &mut measure)?;
            if let Some(mask) = &layer.mask {
                measure.vector(&mask.steps)?;
                for step in &mask.steps {
                    if !step.weight.is_finite() || !step.value.is_finite() {
                        return Err("Nonfinite native mask metadata".into());
                    }
                    step.id.heap(&mut measure)?;
                    step.kind.heap(&mut measure)?;
                    raster_heap(&step.pixels, &mut measure)?;
                }
            }
            effect_heap(&layer.effects, &mut measure)?;
        }
        effect_heap(&self.filters, &mut measure)?;
        for coverage in [
            self.selection_coverage.as_ref(),
            self.selection_previous.as_ref(),
        ]
        .into_iter()
        .flatten()
        {
            raster_heap(&coverage.mask, &mut measure)?;
        }
        Ok(measure.bytes())
    }
}
fn bind_coverage(
    value: Coverage36,
    pool: &ResidentBudget,
    check: &dyn Fn() -> Result<()>,
) -> Result<Coverage36> {
    if value.contours.len() > 500_000
        || value
            .contours
            .iter()
            .try_fold(0usize, |n, c| n.checked_add(c.len()))
            .is_none_or(|n| n > 500_000)
    {
        return Err("Native contours exceed their field admission".into());
    }
    Ok(Coverage36 {
        mask: value.mask,
        bounds: value.bounds,
        origin: value.origin,
        contours: value.contours.bind(pool, check)?,
    })
}
fn bind_effect(
    value: Effect36,
    pool: &ResidentBudget,
    check: &dyn Fn() -> Result<()>,
) -> Result<Effect36> {
    Ok(Effect36 {
        id: value.id,
        kind: value.kind,
        enabled: value.enabled,
        weight: value.weight,
        settings: value.settings.bind(pool, check)?,
        clips: owned_vector(value.clips, check, |v| {
            Ok(EffectClip36 {
                stroke: v.stroke,
                coverage: bind_coverage(v.coverage, pool, check)?,
            })
        })?,
    })
}
fn bind_layer(
    value: Layer36,
    pool: &ResidentBudget,
    check: &dyn Fn() -> Result<()>,
) -> Result<Layer36> {
    Ok(Layer36 {
        id: value.id,
        name: value.name,
        kind: value.kind,
        parent: value.parent,
        clip_to: value.clip_to,
        visible: value.visible,
        locked: value.locked,
        opacity: value.opacity,
        blend: value.blend,
        x: value.x,
        y: value.y,
        pixels: value.pixels,
        color: value.color,
        mask: value
            .mask
            .map(|mask| -> Result<_> {
                Ok(Mask36 {
                    enabled: mask.enabled,
                    linked: mask.linked,
                    steps: owned_vector(mask.steps, check, |step| {
                        Ok(MaskStep36 {
                            id: step.id,
                            kind: step.kind,
                            enabled: step.enabled,
                            weight: step.weight,
                            value: step.value,
                            pixels: step.pixels,
                            settings: step.settings.bind(pool, check)?,
                        })
                    })?,
                })
            })
            .transpose()?,
        effects: owned_vector(value.effects, check, |v| bind_effect(v, pool, check))?,
        psd_metadata: value.psd_metadata.bind(pool, check)?,
        source: value
            .source
            .map(|v| -> Result<_> {
                Ok(Source36 {
                    width: v.width,
                    height: v.height,
                    matrix: v.matrix,
                    content: v.content.bind(pool, check)?,
                })
            })
            .transpose()?,
    })
}
fn raster_heap(value: &RasterRef36, measure: &mut Measure<'_>) -> Result<()> {
    measure.node()?;
    if let Some(original) = &value.retained {
        if original.pixels.retained.is_some() {
            return Err("Nested native original".into());
        }
        measure.add(std::mem::size_of::<Retained36>() + 32)?;
        if original
            .matrix
            .iter()
            .chain(original.projective.iter().flatten())
            .any(|v| !v.is_finite())
            || original.warps.iter().any(|w| {
                w.before
                    .iter()
                    .chain(w.grid.rect.iter())
                    .chain(w.grid.points.iter().flatten())
                    .any(|v| !v.is_finite())
            })
        {
            return Err("Nonfinite retained native metadata".into());
        }
        measure.vector(&original.warps)?;
    }
    Ok(())
}
fn effect_heap(values: &Vec<Effect36>, measure: &mut Measure<'_>) -> Result<()> {
    measure.vector(values)?;
    for value in values {
        if !value.weight.is_finite() {
            return Err("Nonfinite native effect metadata".into());
        }
        value.id.heap(measure)?;
        value.kind.heap(measure)?;
        measure.vector(&value.clips)?;
        for clip in &value.clips {
            raster_heap(&clip.coverage.mask, measure)?;
        }
    }
    Ok(())
}
#[cfg(test)]
#[path = "metadata_tests.rs"]
mod tests;
