//! One immutable candidate for every source and metadata change in a command.
use super::*;
use crate::engine::scene::{Coverage36, Retained36, Source36};

pub(crate) struct RasterBatch {
    pub rasters: Vec<RasterPatch>,
    pub layers: Vec<LayerPatch>,
    pub effects: Vec<EffectPatch>,
    /// None preserves selection. Some replaces all four selection fields.
    pub selection: Option<SelectionPatch>,
}
pub(crate) struct LayerPatch {
    /// The primary color slot identifies the layer in the captured generation.
    pub target: RasterSlot,
    pub origin: Option<[i32; 2]>,
    pub source: SourceUpdate,
    /// Explicit fill -> paint transition, paired with a complete raster output.
    pub materialize_fill: bool,
}
pub(crate) enum SourceUpdate {
    Keep,
    Clear,
    Matrix([f32; 6]),
    Replace(Admitted<Source36>),
}
pub(crate) struct SelectionPatch {
    pub bounds: Option<[i32; 4]>,
    pub polygon: Option<Admitted<Vec<[f32; 2]>>>,
    pub current: Option<CoveragePatch>,
    pub previous: Option<CoveragePatch>,
}
pub(crate) struct CoveragePatch {
    pub pixels: CoveragePixels,
    pub bounds: [i32; 4],
    pub origin: [i32; 2],
    pub contours: Admitted<Vec<Vec<[f32; 2]>>>,
}
pub(crate) enum CoveragePixels {
    Existing(RasterSlot),
    Output(StagedOutput),
}
#[must_use]
pub(crate) struct RefusedBatch {
    reason: String,
    batch: RasterBatch,
}
impl RefusedBatch {
    pub fn reason(&self) -> &str {
        &self.reason
    }
    pub fn into_batch(self) -> RasterBatch {
        self.batch
    }
    pub fn abort(
        self,
        session: &impl super::super::scene::StageSession,
        cleanup: &dyn Fn() -> Result<()>,
    ) -> Result<()> {
        self.batch.abort(session, cleanup)
    }
}
impl RasterBatch {
    pub(crate) fn abort(
        self,
        session: &impl super::super::scene::StageSession,
        cleanup: &dyn Fn() -> Result<()>,
    ) -> Result<()> {
        let effect_count = self
            .effects
            .iter()
            .try_fold(0usize, |n, patch| {
                n.checked_add(patch.clips.as_ref().map_or(0, Vec::len))
            })
            .ok_or("Native clip count overflow")?;
        let count = self
            .rasters
            .len()
            .checked_mul(2)
            .and_then(|n| n.checked_add(2))
            .and_then(|n| n.checked_add(effect_count))
            .ok_or("Native retirement footprint overflow")?;
        let mut outputs = Vec::new();
        outputs.try_reserve_exact(count).map_err(|_| {
            "Native retirement allocation refused; provisional bytes remain charged"
        })?;
        for raster in self.rasters {
            outputs.push(raster.output);
            if let RetainedUpdate::ReplaceOutput { source, .. } = raster.retained {
                outputs.push(source);
            }
        }
        if let Some(selection) = self.selection {
            for coverage in [selection.current, selection.previous]
                .into_iter()
                .flatten()
            {
                if let CoveragePixels::Output(output) = coverage.pixels {
                    outputs.push(output);
                }
            }
        }
        for patch in self.effects {
            if let Some(clips) = patch.clips {
                for clip in clips {
                    if let CoveragePixels::Output(output) = clip.coverage.pixels {
                        outputs.push(output);
                    }
                }
            }
        }
        super::output::abort_outputs(outputs, session, cleanup)
    }
}
impl CapturedEdit {
    pub(crate) fn batch(
        self,
        batch: RasterBatch,
        check: &dyn Fn() -> Result<()>,
    ) -> std::result::Result<PreparedDelta, RefusedBatch> {
        let checked = || {
            self.cancellation.check()?;
            check()?;
            self.cancellation.check()
        };
        // The producer owns/admitted the input arrays. All additional preparation
        // arrays and deep copies are covered before their first allocation below.
        match self.prepare_batch(
            &batch.rasters,
            &batch.layers,
            batch.selection.as_ref(),
            &batch.effects,
            &checked,
        ) {
            Err(reason) => Err(RefusedBatch { reason, batch }),
            Ok(mut prepared) => {
                let outputs = batch
                    .effects
                    .iter()
                    .map(|patch| {
                        patch.clips.as_ref().map_or(0, |clips| {
                            clips
                                .iter()
                                .filter(|clip| {
                                    matches!(clip.coverage.pixels, CoveragePixels::Output(_))
                                })
                                .count()
                        })
                    })
                    .sum::<usize>()
                    + batch
                        .rasters
                        .iter()
                        .map(|raster| {
                            if matches!(raster.retained, RetainedUpdate::ReplaceOutput { .. }) {
                                2
                            } else {
                                1
                            }
                        })
                        .sum::<usize>()
                    + batch.selection.as_ref().map_or(0, |selection| {
                        [&selection.current, &selection.previous]
                            .into_iter()
                            .flatten()
                            .filter(|coverage| matches!(coverage.pixels, CoveragePixels::Output(_)))
                            .count()
                    });
                if prepared.outputs.try_reserve_exact(outputs).is_err() {
                    return Err(RefusedBatch {
                        reason: "Native output owner allocation refused".into(),
                        batch,
                    });
                }
                for raster in batch.rasters {
                    prepared.outputs.push(raster.output);
                    if let RetainedUpdate::ReplaceOutput { source, .. } = raster.retained {
                        prepared.outputs.push(source);
                    }
                }
                if let Some(selection) = batch.selection {
                    for coverage in [selection.current, selection.previous]
                        .into_iter()
                        .flatten()
                    {
                        if let CoveragePixels::Output(output) = coverage.pixels {
                            prepared.outputs.push(output);
                        }
                    }
                }
                for patch in batch.effects {
                    if let Some(clips) = patch.clips {
                        for clip in clips {
                            if let CoveragePixels::Output(output) = clip.coverage.pixels {
                                prepared.outputs.push(output);
                            }
                        }
                    }
                }
                Ok(prepared)
            }
        }
    }
    pub(super) fn prepare_batch<'a>(
        &'a self,
        rasters: &'a [RasterPatch],
        layers: &'a [LayerPatch],
        selection: Option<&'a SelectionPatch>,
        effects: &'a [EffectPatch],
        check: &dyn Fn() -> Result<()>,
    ) -> Result<PreparedDelta> {
        check()?;
        if rasters.is_empty() && layers.is_empty() && selection.is_none() && effects.is_empty() {
            return Err("Native edit batch is empty".into());
        }
        let base = &self.base.core.metadata;
        let pool = self.base.budget();
        let locations = self.base.roster().len();
        let (effect_bytes, effect_locations) = super::effect_patch::admission(&self.base, effects)?;
        let added = rasters
            .len()
            .checked_mul(2)
            .and_then(|n| n.checked_add(4))
            .and_then(|n| n.checked_add(effect_locations))
            .ok_or("Native batch footprint overflow")?;
        if locations
            .checked_add(added)
            .is_none_or(|n| n > super::super::scene::MAX_RASTER_LOCATIONS)
        {
            return Err("Native batch source count exceeds its bound".into());
        }
        let mut bytes = (rasters.len() + layers.len() + 4)
            .checked_mul(32 * 1024)
            .and_then(|n| n.checked_add(effect_bytes))
            .ok_or("Native batch footprint overflow")?;
        for layer in layers {
            if let SourceUpdate::Replace(source) = &layer.source {
                if !source.owner().uses_budget(pool) {
                    return Err("Source definition uses another admission".into());
                }
                bytes = bytes
                    .checked_add(source.owner().bytes())
                    .ok_or("Native batch footprint overflow")?;
            }
        }
        if let Some(selection) = selection {
            if let Some(polygon) = &selection.polygon {
                if !polygon.owner().uses_budget(pool) {
                    return Err("Selection polygon uses another admission".into());
                }
                bytes = bytes
                    .checked_add(polygon.owner().bytes())
                    .ok_or("Native batch footprint overflow")?;
            }
            for coverage in [&selection.current, &selection.previous]
                .into_iter()
                .flatten()
            {
                if !coverage.contours.owner().uses_budget(pool) {
                    return Err("Selection contours use another admission".into());
                }
                bytes = bytes
                    .checked_add(coverage.contours.owner().bytes())
                    .ok_or("Native batch footprint overflow")?;
            }
        }
        for ordinal in rasters
            .iter()
            .map(|patch| patch.target.ordinal)
            .chain(layers.iter().map(|patch| patch.target.ordinal))
        {
            if let Some(layer) = base
                .layers
                .iter()
                .find(|layer| super::patch::layer_contains(layer, ordinal))
            {
                bytes = bytes
                    .checked_add(layer.id.len())
                    .ok_or("Native scope footprint overflow")?;
            }
        }
        let working = MetadataCharge::reserve(pool, bytes, check)?;
        let mut replacements: Vec<(u32, RasterRef36)> = Vec::new();
        replacements
            .try_reserve_exact(rasters.len())
            .map_err(|_| "Native patch allocation refused")?;
        let mut outputs: Vec<&Surface> = Vec::new();
        outputs
            .try_reserve_exact(rasters.len() * 2 + 2 + effect_locations)
            .map_err(|_| "Native output index allocation refused")?;
        let mut scopes = Vec::new();
        scopes
            .try_reserve_exact(rasters.len() + layers.len() + effects.len() + 1)
            .map_err(|_| "Native scope allocation refused")?;
        for (index, patch) in rasters.iter().enumerate() {
            check()?;
            if rasters[..index]
                .iter()
                .any(|other| other.target == patch.target)
            {
                return Err("Duplicate native raster target".into());
            }
            let old = self.base.source(patch.target)?;
            let output = patch.output.surface();
            if output.depth() != old.depth()
                || !output.uses_budget(pool)
                || !output.uses_disk_budget(self.base.disk_budget())
            {
                return Err("Native output depth or admission differs".into());
            }
            let original = reference(base, patch.target.ordinal)?;
            let layer = base
                .layers
                .iter()
                .find(|layer| super::patch::layer_contains(layer, patch.target.ordinal));
            if let Some(layer) = layer {
                super::patch::unlocked(base, &layer.id)?;
                let changed = !super::patch::same_current(old, output);
                if layer.pixels.id == patch.target.ordinal
                    && layer.source.is_some()
                    && changed
                    && !layers.iter().any(|update| {
                        update.target == patch.target
                            && !matches!(update.source, SourceUpdate::Keep)
                    })
                {
                    return Err("Native editable source pixels require an explicit source-definition update".into());
                }
                scopes.push(layer.id.clone());
            } else {
                scopes.push("@selection".into());
            }
            if matches!(patch.retained, RetainedUpdate::Keep)
                && original.retained.is_some()
                && !super::patch::same_current(old, output)
            {
                return Err(
                    "Changed retained pixels require explicit source reconciliation".into(),
                );
            }
            if let Some(origin) = patch.layer_origin {
                valid_origin(origin)?;
                if !layer.is_some_and(|layer| layer.pixels.id == patch.target.ordinal) {
                    return Err("Native origin does not match a primary layer slot".into());
                }
                if layers
                    .iter()
                    .any(|update| update.target == patch.target && update.origin.is_some())
                {
                    return Err("Ambiguous native layer origin update".into());
                }
            }
            // Current output and original descriptor are separate. References to
            // old sources are resolved against the captured roster, never against
            // another output's provisional/renumbered ordinal.
            let retained = retained(
                &self.base,
                original,
                &patch.retained,
                output.depth(),
                &mut outputs,
            )?;
            let id = u32::try_from(locations + outputs.len() + 1)
                .map_err(|_| "Native source ordinal overflow")?;
            outputs.push(output);
            replacements.push((
                patch.target.ordinal,
                RasterRef36 {
                    id,
                    width: output.width(),
                    height: output.height(),
                    depth: output.depth(),
                    retained,
                },
            ));
        }
        for (index, update) in layers.iter().enumerate() {
            self.base.source(update.target)?;
            if layers[..index]
                .iter()
                .any(|old| old.target == update.target)
            {
                return Err("Duplicate native layer metadata target".into());
            }
            let layer = base
                .layers
                .iter()
                .find(|layer| layer.pixels.id == update.target.ordinal)
                .ok_or("Layer update requires a primary color slot")?;
            super::patch::unlocked(base, &layer.id)?;
            if let Some(origin) = update.origin {
                valid_origin(origin)?;
            }
            if update.materialize_fill
                && (layer.kind != "fill"
                    || !rasters.iter().any(|patch| patch.target == update.target))
            {
                return Err("Fill materialization requires its complete raster output".into());
            }
            match &update.source {
                SourceUpdate::Matrix(matrix) => {
                    if layer.source.is_none() {
                        return Err("Layer has no editable source definition".into());
                    }
                    valid_matrix(*matrix)?;
                    if layer
                        .source
                        .as_ref()
                        .is_some_and(|source| source.matrix != *matrix)
                        && !rasters.iter().any(|patch| patch.target == update.target)
                    {
                        return Err(
                            "Changed source transform requires its rendered raster output".into(),
                        );
                    }
                }
                SourceUpdate::Replace(source) => {
                    if !rasters.iter().any(|patch| patch.target == update.target) {
                        return Err(
                            "Changed source definition requires its rendered raster output".into(),
                        );
                    }
                    let admitted: Admitted<crate::source::Source> =
                        copy_admitted(&**source, pool, false, check)?;
                    // Preserve the current validated source contract, including
                    // font/SVG/path checks; this does not lift its existing caps.
                    let validation = MetadataCharge::reserve(pool, 32 * 1024 * 1024, check)?;
                    admitted.validate()?;
                    drop(validation);
                }
                _ => {}
            }
            scopes.push(layer.id.clone());
        }
        let global = super::effect_patch::scopes(&self.base, effects, &mut scopes)?;
        if selection.is_some() {
            scopes.push("@selection".into());
        }
        scopes.sort_unstable();
        scopes.dedup();
        let intent = if global {
            Intent::from_scopes(
                IntentKind::Placement,
                &[crate::engine::Scope {
                    target: None,
                    rect: None,
                }],
                pool,
            )?
        } else {
            Intent::layers(
                if selection.is_some() {
                    IntentKind::Selection
                } else {
                    IntentKind::Placement
                },
                &scopes,
                pool,
            )?
        };
        let copied = copy_metadata(base, pool, check)?;
        let mut prepared_core = None;
        copied.map_unique(|mut metadata, _| {
            // Update metadata targets before changing any captured ordinal.
            for update in layers {
                let layer = metadata
                    .layers
                    .iter_mut()
                    .find(|layer| layer.pixels.id == update.target.ordinal)
                    .ok_or("Native layer disappeared")?;
                if let Some(origin) = update.origin {
                    layer.x = origin[0];
                    layer.y = origin[1];
                }
                if update.materialize_fill {
                    layer.kind = "paint".into();
                }
                match &update.source {
                    SourceUpdate::Keep => {}
                    SourceUpdate::Clear => layer.source = None,
                    SourceUpdate::Matrix(matrix) => {
                        layer
                            .source
                            .as_mut()
                            .ok_or("Missing native source definition")?
                            .matrix = *matrix
                    }
                    SourceUpdate::Replace(source) => layer.source = Some((**source).clone()),
                }
            }
            for patch in rasters {
                if let Some(origin) = patch.layer_origin {
                    let layer = metadata
                        .layers
                        .iter_mut()
                        .find(|layer| layer.pixels.id == patch.target.ordinal)
                        .ok_or("Native origin target disappeared")?;
                    layer.x = origin[0];
                    layer.y = origin[1];
                }
            }
            // A replacement may retain a captured source that is itself being
            // changed elsewhere in this batch. Freeze that original unchanged;
            // do not recursively apply another replacement to its old ordinal.
            replace_slots(&mut metadata, &replacements);
            super::effect_patch::apply(&self.base, &mut metadata, effects, &mut outputs)?;
            if let Some(selection) = selection {
                valid_selection(selection)?;
                metadata.selection = selection.bounds;
                metadata.selection_polygon = selection
                    .polygon
                    .as_ref()
                    .map(|polygon| (**polygon).clone());
                metadata.selection_coverage = selection
                    .current
                    .as_ref()
                    .map(|coverage| coverage_ref(&self.base, coverage, &mut outputs))
                    .transpose()?;
                metadata.selection_previous = selection
                    .previous
                    .as_ref()
                    .map(|coverage| coverage_ref(&self.base, coverage, &mut outputs))
                    .transpose()?;
            }
            let roster = rebuild_from(
                &mut metadata,
                locations + added,
                pool,
                self.base.disk_budget(),
                check,
                |reference| {
                    if reference.id as usize > locations {
                        let surface = outputs
                            .get(reference.id as usize - locations - 1)
                            .ok_or("Missing generated native source")?;
                        if surface.width() != reference.width
                            || surface.height() != reference.height
                            || surface.depth() != reference.depth
                        {
                            return Err("Generated native source shape differs".into());
                        }
                        Ok((*surface).clone())
                    } else {
                        self.base.roster().get(reference).cloned()
                    }
                },
            )?;
            metadata.revision = metadata
                .revision
                .checked_add(1)
                .ok_or("Native revision overflow")?;
            let fresh = copy_metadata(&metadata, pool, check)?;
            prepared_core = Some(fresh.map_unique(|metadata, _| {
                Ok(Core {
                    metadata,
                    roster,
                    resident: pool.clone(),
                    disk: self.base.disk_budget().clone(),
                    key: key(),
                    render_key: OnceLock::new(),
                })
            })?);
            Ok(())
        })?;
        drop(scopes);
        drop(outputs);
        drop(replacements);
        drop(working);
        check()?;
        Ok(PreparedDelta {
            base: self.base.key(),
            candidate: NativeDocument {
                core: prepared_core.ok_or("Native batch handoff failed")?,
            },
            cancellation: self.cancellation.clone(),
            intent,
            outputs: vec![],
            admission: self.admission.clone(),
        })
    }
}
fn valid_origin(origin: [i32; 2]) -> Result<()> {
    if origin.iter().any(|v| v.unsigned_abs() > 100_000) {
        Err("Invalid native layer origin".into())
    } else {
        Ok(())
    }
}
fn valid_matrix(matrix: [f32; 6]) -> Result<()> {
    let [a, b, c, d, _, _] = matrix;
    if matrix.iter().any(|v| !v.is_finite() || v.abs() > 100_000.)
        || (a * d - b * c).abs() < 0.000001
    {
        Err("Invalid editable-source transform".into())
    } else {
        Ok(())
    }
}
fn reference(metadata: &Document36, ordinal: u32) -> Result<&RasterRef36> {
    let mut found = None;
    metadata.visit_rasters(|reference| {
        if reference.id == ordinal {
            found = Some(reference);
        }
        Ok(())
    })?;
    found.ok_or_else(|| "Missing native raster target".into())
}
fn retained<'a>(
    base: &NativeDocument,
    old: &RasterRef36,
    update: &'a RetainedUpdate,
    depth: u16,
    outputs: &mut Vec<&'a Surface>,
) -> Result<Option<Box<Retained36>>> {
    let (original, ordinal, matrix, projective, warps) = match update {
        RetainedUpdate::Keep => return Ok(old.retained.clone()),
        RetainedUpdate::Clear => return Ok(None),
        RetainedUpdate::Replace {
            source,
            matrix,
            projective,
            warps,
        } => (
            base.source(*source)?,
            source.ordinal,
            matrix,
            projective,
            warps,
        ),
        RetainedUpdate::ReplaceOutput {
            source,
            matrix,
            projective,
            warps,
        } => {
            let surface = source.surface();
            if !surface.uses_budget(base.budget()) || !surface.uses_disk_budget(base.disk_budget())
            {
                return Err("Retained output uses another admission".into());
            }
            let ordinal = u32::try_from(base.roster().len() + outputs.len() + 1)
                .map_err(|_| "Native source ordinal overflow")?;
            outputs.push(surface);
            (surface, ordinal, matrix, projective, warps)
        }
    };
    if warps.len() > crate::warp::MAX_STAGES {
        return Err("Retained Warp stage limit exceeded".into());
    }
    let mut stages = Vec::new();
    stages
        .try_reserve_exact(warps.len())
        .map_err(|_| "Retained placement allocation refused")?;
    for warp in warps {
        stages.push(crate::warp::Stage {
            before: warp.before,
            grid: crate::warp::Grid {
                rect: warp.grid.rect,
                points: warp.grid.points,
            },
        });
    }
    crate::retained::geometry::validate(
        original.width(),
        original.height(),
        original.depth(),
        depth,
        *matrix,
        *projective,
        &stages,
    )?;
    Ok(Some(Box::new(Retained36 {
        pixels: RasterRef36 {
            id: ordinal,
            width: original.width(),
            height: original.height(),
            depth: original.depth(),
            retained: None,
        },
        matrix: *matrix,
        projective: *projective,
        warps: warps.clone(),
    })))
}

fn replace_slots(metadata: &mut Document36, replacements: &[(u32, RasterRef36)]) {
    fn replace(reference: &mut RasterRef36, replacements: &[(u32, RasterRef36)]) {
        if let Some((_, new)) = replacements.iter().find(|(id, _)| *id == reference.id) {
            *reference = new.clone();
        } else if let Some(original) = &mut reference.retained {
            if let Some((_, new)) = replacements
                .iter()
                .find(|(id, _)| *id == original.pixels.id)
            {
                original.pixels = new.clone();
            }
        }
    }
    for layer in &mut metadata.layers {
        replace(&mut layer.pixels, replacements);
        if let Some(mask) = &mut layer.mask {
            for step in &mut mask.steps {
                replace(&mut step.pixels, replacements);
            }
        }
        for effect in &mut layer.effects {
            for clip in &mut effect.clips {
                replace(&mut clip.coverage.mask, replacements);
            }
        }
    }
    for effect in &mut metadata.filters {
        for clip in &mut effect.clips {
            replace(&mut clip.coverage.mask, replacements);
        }
    }
    for coverage in [
        &mut metadata.selection_coverage,
        &mut metadata.selection_previous,
    ]
    .into_iter()
    .flatten()
    {
        replace(&mut coverage.mask, replacements);
    }
}
fn valid_selection(selection: &SelectionPatch) -> Result<()> {
    if selection.bounds.is_some_and(|bounds| {
        bounds.iter().any(|v| v.unsigned_abs() > 100000)
            || bounds[0] > bounds[2]
            || bounds[1] > bounds[3]
    }) {
        return Err("Invalid native selection bounds".into());
    }
    if selection.polygon.as_ref().is_some_and(|polygon| {
        polygon.len() > 8192
            || polygon
                .iter()
                .flatten()
                .any(|v| !v.is_finite() || v.abs() > 100000.)
    }) {
        return Err("Invalid native selection polygon".into());
    }
    Ok(())
}
pub(super) fn coverage_ref<'a>(
    base: &NativeDocument,
    coverage: &'a CoveragePatch,
    outputs: &mut Vec<&'a Surface>,
) -> Result<Coverage36> {
    let mask = match &coverage.pixels {
        CoveragePixels::Existing(slot) => {
            base.source(*slot)?;
            reference(&base.core.metadata, slot.ordinal)?.clone()
        }
        CoveragePixels::Output(output) => {
            let surface = output.surface();
            let id = u32::try_from(base.roster().len() + outputs.len() + 1)
                .map_err(|_| "Native source ordinal overflow")?;
            outputs.push(surface);
            RasterRef36 {
                id,
                width: surface.width(),
                height: surface.height(),
                depth: surface.depth(),
                retained: None,
            }
        }
    };
    if mask.depth != 8
        || coverage
            .bounds
            .iter()
            .chain(coverage.origin.iter())
            .any(|v| v.unsigned_abs() > 100000)
        || coverage.bounds[0] > coverage.bounds[2]
        || coverage.bounds[1] > coverage.bounds[3]
        || coverage.contours.len() > 500000
        || coverage
            .contours
            .iter()
            .try_fold(0usize, |n, line| n.checked_add(line.len()))
            .is_none_or(|n| n > 500000)
        || coverage
            .contours
            .iter()
            .flatten()
            .flatten()
            .any(|v| !v.is_finite() || v.abs() > 100000.)
    {
        return Err("Invalid native selection coverage".into());
    }
    Ok(Coverage36 {
        mask,
        bounds: coverage.bounds,
        origin: coverage.origin,
        contours: (*coverage.contours).clone(),
    })
}

#[cfg(test)]
mod tests {
    use super::super::output::tests::{draft, Session};
    use super::*;
    use crate::engine::scene::{Content36, Mask36, MaskStep36};
    fn admitted<T>(pool: &ResidentBudget, value: T) -> Admitted<T> {
        let owner = MetadataCharge::reserve(pool, 64 * 1024, &|| Ok(())).unwrap();
        Admitted::from_parts(Arc::new(value), owner)
    }
    fn document(pool: &ResidentBudget) -> NativeDocument {
        let base = super::super::tests::document(pool);
        let mut metadata = base.core.metadata.clone();
        metadata.layers[0].source = Some(Source36 {
            width: 8,
            height: 8,
            matrix: [1., 0., 0., 1., 0., 0.],
            content: Content36::Shape {
                shape: "rectangle".into(),
                bounds: [0., 0., 8., 8.],
                points: vec![],
                closed: true,
                fill: [0, 0, 0, 65535],
                stroke: [0; 4],
                stroke_width: 0.,
            },
        });
        metadata.layers[0].mask = Some(Mask36 {
            enabled: true,
            linked: true,
            steps: vec![MaskStep36 {
                id: "mask".into(),
                kind: "paint".into(),
                enabled: true,
                weight: 1.,
                value: 255.,
                pixels: RasterRef36 {
                    id: 2,
                    width: 8,
                    height: 8,
                    depth: 16,
                    retained: None,
                },
                settings: serde_json::Value::Null,
            }],
        });
        metadata.selection_coverage = Some(Coverage36 {
            mask: RasterRef36 {
                id: 3,
                width: 8,
                height: 8,
                depth: 8,
                retained: None,
            },
            bounds: [1, 1, 4, 4],
            origin: [0, 0],
            contours: vec![],
        });
        metadata.selection = Some([1, 1, 4, 4]);
        let roster = RasterRoster::new(vec![
            Surface::zero(8, 8, 16).unwrap(),
            Surface::zero(8, 8, 16).unwrap(),
            Surface::zero(8, 8, 8).unwrap(),
        ])
        .unwrap();
        NativeDocument::from_pending(
            PendingScene::new(
                admitted(pool, metadata),
                roster,
                pool.clone(),
                DiskBudget::new(8 * 1024 * 1024, 64).unwrap(),
            )
            .unwrap(),
        )
        .unwrap()
    }
    fn patch(base: &NativeDocument, target: RasterSlot) -> (RasterPatch, std::path::PathBuf) {
        let (draft, path) = draft(base);
        let output = draft
            .seal(base, &Session, &|| Ok(()))
            .unwrap_or_else(|e| panic!("{}", e.reason()));
        (
            RasterPatch {
                target,
                output,
                retained: RetainedUpdate::Keep,
                layer_origin: None,
            },
            path,
        )
    }
    #[test]
    fn linked_planes_source_placement_and_soft_selection_change_in_one_revision() {
        let pool = ResidentBudget::new(16 * 1024 * 1024).unwrap();
        let base = document(&pool);
        let layer = base.metadata().layers().next().unwrap();
        let color = layer.pixels().slot();
        let mask = layer
            .mask()
            .unwrap()
            .steps()
            .next()
            .unwrap()
            .pixels()
            .slot();
        let old_coverage = base.metadata().selection_coverage().unwrap().mask().slot();
        let (color_patch, p1) = patch(&base, color);
        let (mask_patch, p2) = patch(&base, mask);
        let coverage = OutputDraft::new(Surface::zero(8, 8, 8).unwrap(), vec![])
            .seal(&base, &Session, &|| Ok(()))
            .unwrap_or_else(|e| panic!("{}", e.reason()));
        let batch = RasterBatch {
            effects: vec![],
            rasters: vec![color_patch, mask_patch],
            layers: vec![LayerPatch {
                target: color,
                origin: Some([-4, 7]),
                source: SourceUpdate::Matrix([1., 0., 0., 1., 2., 3.]),
                materialize_fill: false,
            }],
            selection: Some(SelectionPatch {
                bounds: Some([0, 0, 3, 3]),
                polygon: Some(admitted(&pool, vec![[0., 0.], [3., 0.], [0., 3.]])),
                current: Some(CoveragePatch {
                    pixels: CoveragePixels::Output(coverage),
                    bounds: [0, 0, 3, 3],
                    origin: [-1, 2],
                    contours: admitted(&pool, vec![vec![[0., 0.], [3., 0.], [0., 3.]]]),
                }),
                previous: Some(CoveragePatch {
                    pixels: CoveragePixels::Existing(old_coverage),
                    bounds: [1, 1, 4, 4],
                    origin: [0, 0],
                    contours: admitted(&pool, vec![]),
                }),
            }),
        };
        let prepared = base
            .capture_edit()
            .unwrap()
            .batch(batch, &|| Ok(()))
            .unwrap_or_else(|e| panic!("{}", e.reason()));
        assert_eq!(base.metadata().revision(), 0);
        assert_eq!(prepared.preview().metadata().revision(), 1);
        let current = base.publish("human", &[], 1, false, prepared).unwrap();
        let layer = current.metadata().layers().next().unwrap();
        assert_eq!(layer.origin(), [-4, 7]);
        assert_eq!(layer.source().unwrap().matrix, [1., 0., 0., 1., 2., 3.]);
        assert_eq!(current.metadata().selection(), Some([0, 0, 3, 3]));
        assert_eq!(
            current.metadata().selection_previous().unwrap().bounds(),
            [1, 1, 4, 4]
        );
        let mut ordinals = vec![];
        current
            .metadata()
            .visit_rasters(|r| {
                ordinals.push(r.ordinal());
                Ok(())
            })
            .unwrap();
        assert_eq!(ordinals, vec![1, 2, 3, 4]);
        for slot in [
            layer.pixels().slot(),
            layer
                .mask()
                .unwrap()
                .steps()
                .next()
                .unwrap()
                .pixels()
                .slot(),
        ] {
            let region = current
                .source(slot)
                .unwrap()
                .prepare_region(
                    crate::raster::backed::Region::new([0, 0, 1, 1]).unwrap(),
                    &mut || Ok(()),
                )
                .unwrap();
            assert_eq!(region.pixel16(0, 0).unwrap(), [12345, 23456, 34567, 65535]);
        }
        drop(current);
        drop(base);
        std::fs::remove_dir_all(p1).unwrap();
        std::fs::remove_dir_all(p2).unwrap();
        assert_eq!(pool.used(), 0);
    }
    #[test]
    fn a_late_conflict_refuses_every_output_and_registered_cleanup_releases_them() {
        let pool = ResidentBudget::new(16 * 1024 * 1024).unwrap();
        let base = document(&pool);
        let layer = base.metadata().layers().next().unwrap();
        let color = layer.pixels().slot();
        let mask = layer
            .mask()
            .unwrap()
            .steps()
            .next()
            .unwrap()
            .pixels()
            .slot();
        let (first, p1) = patch(&base, color);
        let (second, p2) = patch(&base, mask);
        let batch = RasterBatch {
            effects: vec![],
            rasters: vec![first, second],
            layers: vec![LayerPatch {
                target: color,
                origin: None,
                source: SourceUpdate::Matrix([1., 0., 0., 1., 0., 0.]),
                materialize_fill: false,
            }],
            selection: None,
        };
        let prepared = base
            .capture_edit()
            .unwrap()
            .batch(batch, &|| Ok(()))
            .unwrap_or_else(|e| panic!("{}", e.reason()));
        let lease = crate::engine::Lease {
            id: "late".into(),
            owner: "other".into(),
            description: "linked masks".into(),
            scopes: vec![crate::engine::Scope::layer(layer.id())],
            expires: 100,
        };
        let refused = base
            .publish("worker", &[lease], 1, false, prepared)
            .unwrap_err();
        assert_eq!(base.metadata().revision(), 0);
        refused.abort(&Session, &|| Ok(())).unwrap();
        assert_eq!(base.disk_budget().used().unwrap(), 0);
        std::fs::remove_dir(p1).unwrap();
        std::fs::remove_dir(p2).unwrap();
    }
    #[test]
    fn ambiguous_targets_and_invalid_source_updates_return_all_output_tokens() {
        let pool = ResidentBudget::new(16 * 1024 * 1024).unwrap();
        let base = document(&pool);
        let target = base.metadata().layers().next().unwrap().pixels().slot();
        let (first, p1) = patch(&base, target);
        let (second, p2) = patch(&base, target);
        let batch = RasterBatch {
            effects: vec![],
            rasters: vec![first, second],
            layers: vec![LayerPatch {
                target,
                origin: None,
                source: SourceUpdate::Matrix([0.; 6]),
                materialize_fill: false,
            }],
            selection: None,
        };
        let refused = base
            .capture_edit()
            .unwrap()
            .batch(batch, &|| Ok(()))
            .err()
            .unwrap();
        assert!(refused.reason().contains("Duplicate"));
        refused.abort(&Session, &|| Ok(())).unwrap();
        assert_eq!(base.disk_budget().used().unwrap(), 0);
        std::fs::remove_dir(p1).unwrap();
        std::fs::remove_dir(p2).unwrap();
        let batch = RasterBatch {
            effects: vec![],
            rasters: vec![],
            layers: vec![LayerPatch {
                target,
                origin: None,
                source: SourceUpdate::Matrix([0.; 6]),
                materialize_fill: false,
            }],
            selection: None,
        };
        assert!(base
            .capture_edit()
            .unwrap()
            .batch(batch, &|| Ok(()))
            .err()
            .unwrap()
            .reason()
            .contains("Invalid editable-source"));
    }
    #[test]
    fn fill_materialization_is_explicit_and_source_replacement_keeps_validated_definitions() {
        let pool = ResidentBudget::new(64 * 1024 * 1024).unwrap();
        let base = document(&pool);
        let target = base.metadata().layers().next().unwrap().pixels().slot();
        let source = Source36 {
            width: 8,
            height: 8,
            matrix: [1., 0., 0., 1., 0., 0.],
            content: Content36::Shape {
                shape: "rectangle".into(),
                bounds: [0., 0., 4., 4.],
                points: vec![],
                closed: true,
                fill: [123, 456, 789, 65535],
                stroke: [0; 4],
                stroke_width: 0.,
            },
        };
        let (output, path) = patch(&base, target);
        let batch = RasterBatch {
            effects: vec![],
            rasters: vec![output],
            layers: vec![LayerPatch {
                target,
                origin: None,
                source: SourceUpdate::Replace(admitted(&pool, source)),
                materialize_fill: false,
            }],
            selection: None,
        };
        let prepared = base
            .capture_edit()
            .unwrap()
            .batch(batch, &|| Ok(()))
            .unwrap_or_else(|e| panic!("{}", e.reason()));
        let current = base.publish("human", &[], 1, false, prepared).unwrap();
        assert!(matches!(
            current
                .metadata()
                .layers()
                .next()
                .unwrap()
                .source()
                .unwrap()
                .content,
            Content36::Shape {
                bounds: [0., 0., 4., 4.],
                ..
            }
        ));
        let batch = RasterBatch {
            effects: vec![],
            rasters: vec![],
            layers: vec![LayerPatch {
                target,
                origin: None,
                source: SourceUpdate::Keep,
                materialize_fill: true,
            }],
            selection: None,
        };
        assert!(base
            .capture_edit()
            .unwrap()
            .batch(batch, &|| Ok(()))
            .is_err());
        drop(current);
        drop(base);
        std::fs::remove_dir_all(path).unwrap();
    }
    #[test]
    fn generated_original_and_current_output_have_one_publication_and_refusal_cleanup() {
        let pool = ResidentBudget::new(16 * 1024 * 1024).unwrap();
        let base = document(&pool);
        let target = base.metadata().layers().next().unwrap().pixels().slot();
        let (mut current, p1) = patch(&base, target);
        let (original, p2) = draft(&base);
        let original = original
            .seal(&base, &Session, &|| Ok(()))
            .unwrap_or_else(|e| panic!("{}", e.reason()));
        current.retained = RetainedUpdate::ReplaceOutput {
            source: original,
            matrix: crate::retained::IDENTITY,
            projective: None,
            warps: vec![],
        };
        let batch = RasterBatch {
            effects: vec![],
            rasters: vec![current],
            layers: vec![LayerPatch {
                target,
                origin: None,
                source: SourceUpdate::Clear,
                materialize_fill: false,
            }],
            selection: None,
        };
        let prepared = base
            .capture_edit()
            .unwrap()
            .batch(batch, &|| Ok(()))
            .unwrap_or_else(|e| panic!("{}", e.reason()));
        let raster = prepared
            .preview()
            .metadata()
            .layers()
            .next()
            .unwrap()
            .pixels();
        assert_eq!(raster.ordinal(), 1);
        assert_eq!(raster.retained().unwrap().pixels().ordinal(), 2);
        assert!(!prepared
            .preview()
            .source(raster.retained().unwrap().pixels().slot())
            .unwrap()
            .is_known_zero());
        // The final gate must refuse both original and current generated owners.
        let refused = base.publish("human", &[], 1, true, prepared).unwrap_err();
        refused.abort(&Session, &|| Ok(())).unwrap();
        assert_eq!(base.disk_budget().used().unwrap(), 0);
        std::fs::remove_dir(p1).unwrap();
        std::fs::remove_dir(p2).unwrap();
    }
    #[test]
    fn cancellation_and_foreign_selection_metadata_leave_all_original_sources_untouched() {
        let pool = ResidentBudget::new(16 * 1024 * 1024).unwrap();
        let base = document(&pool);
        let target = base.metadata().layers().next().unwrap().pixels().slot();
        let (patch, path) = patch(&base, target);
        let foreign = ResidentBudget::new(1024 * 1024).unwrap();
        let batch = RasterBatch {
            effects: vec![],
            rasters: vec![patch],
            layers: vec![],
            selection: Some(SelectionPatch {
                bounds: None,
                polygon: Some(admitted(&foreign, vec![])),
                current: None,
                previous: None,
            }),
        };
        let refused = base
            .capture_edit()
            .unwrap()
            .batch(batch, &|| Ok(()))
            .err()
            .unwrap();
        assert!(refused.reason().contains("another admission"));
        refused.abort(&Session, &|| Ok(())).unwrap();
        assert_eq!(base.disk_budget().used().unwrap(), 0);
        std::fs::remove_dir(path).unwrap();
        let captured = base.capture_edit().unwrap();
        captured.cancellation().cancel();
        let batch = RasterBatch {
            effects: vec![],
            rasters: vec![],
            layers: vec![],
            selection: Some(SelectionPatch {
                bounds: None,
                polygon: None,
                current: None,
                previous: None,
            }),
        };
        assert!(captured
            .batch(batch, &|| Ok(()))
            .err()
            .unwrap()
            .reason()
            .contains("cancel"));
        assert_eq!(base.metadata().revision(), 0);
        assert_eq!(base.metadata().selection(), Some([1, 1, 4, 4]));
    }
}
