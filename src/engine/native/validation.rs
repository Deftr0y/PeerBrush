//! Semantic admission for decoded native work, without a resident pixel copy.
use super::*;
use crate::engine::scene::{Coverage36, Effect36};

impl NativeDocument {
    /// Source framing/ownership and the final workspace fence remain separate.
    /// Run off-lock: definition validation may prepare bounded derived metadata.
    pub(crate) fn validate_editable(&self, check: &dyn Fn() -> Result<()>) -> Result<()> {
        validate_editable(
            &self.core.metadata,
            self.roster(),
            self.budget(),
            self.key(),
            check,
        )
    }
}

pub(in crate::engine) fn validate_editable(
    doc: &Document36,
    roster: &RasterRoster,
    pool: &ResidentBudget,
    binding: DocumentKey,
    check: &dyn Fn() -> Result<()>,
) -> Result<()> {
    check()?;
    crate::raster::CanvasLimits::Native.check(doc.width, doc.height)?;
    if doc.read_only || doc.icc_profile.is_some() || ![8, 16].contains(&doc.bit_depth) {
        return Err(
            "Native adoption requires supported editable native-depth work; sources retained"
                .into(),
        );
    }
    if doc.srgb_tagged && doc.psd_intentionally_untagged {
        return Err(crate::psd::CONFLICTING_PROFILE_DECLARATIONS.into());
    }
    if doc.id.is_empty() || doc.revision == u64::MAX || doc.layers.len() > 100 {
        return Err("Native document identity, revision or layer count is unsupported".into());
    }
    if let Some(mask) = &doc.psd_global_mask {
        mask.validate()?;
    }
    if let Some(resolution) = &doc.psd_resolution {
        crate::psd::parse_resolution(resolution)?;
    }
    if let Some(scale) = &doc.psd_print_scale {
        crate::psd::parse_print_scale(scale)?;
    }
    if let Some(aspect) = &doc.psd_pixel_aspect {
        crate::psd::parse_pixel_aspect(aspect)?;
    }
    if let Some(grid) = &doc.psd_grid {
        crate::psd::parse_grid(grid)?;
    }
    crate::psd::validate_photoshop_layer_ids(doc.layers.iter().map(|layer| layer.psd_metadata.as_slice()))?;
    for (at, layer) in doc.layers.iter().enumerate() {
        check()?;
        if layer.id.is_empty()
            || doc.layers[..at].iter().any(|l| l.id == layer.id)
            || !["paint", "fill", "group", "adjustment"].contains(&layer.kind.as_str())
            || !crate::raster::layer_blends(&layer.kind).any(|(b, _)| b == layer.blend)
            || !layer.opacity.is_finite()
            || !(0.0..=1.0).contains(&layer.opacity)
            || layer.x.unsigned_abs() > 100000
            || layer.y.unsigned_abs() > 100000
            || layer.pixels.depth != doc.bit_depth
        {
            return Err(
                "Invalid native layer identity, kind, blend, placement or precision".into(),
            );
        }
        let mut parent = layer.parent.as_deref();
        let mut depth = 0;
        while let Some(id) = parent {
            check()?;
            depth += 1;
            if depth > 16 {
                return Err("Cyclic or excessive native group nesting".into());
            }
            parent = doc
                .layers
                .iter()
                .find(|l| l.id == id && l.kind == "group")
                .ok_or("Invalid native group parent")?
                .parent
                .as_deref();
        }
        if layer.kind == "adjustment"
            && layer.mask.is_none()
            && (layer.x != 0
                || layer.y != 0
                || layer.pixels.width != doc.width
                || layer.pixels.height != doc.height)
        {
            return Err("Adjustment layers use document coordinates".into());
        }
        crate::psd::validate_layer_metadata(&layer.psd_metadata, layer.locked, layer.kind == "group")?;
        if let Some(source) = &layer.source {
            if layer.kind != "paint" {
                return Err("Editable definitions require paint projections".into());
            }
            crate::source::prepared::validate_definition(source, pool, &mut || check())?;
        }
        validate_effects(&layer.effects, pool, doc.bit_depth, false, check)?;
        if let Some(mask) = &layer.mask {
            if mask.steps.len() > 32 {
                return Err("Too many native mask steps".into());
            }
            for (at, step) in mask.steps.iter().enumerate() {
                check()?;
                crate::effects::validate_weight(step.weight)?;
                if step.id.is_empty()
                    || mask.steps[..at].iter().any(|s| s.id == step.id)
                    || step.pixels.depth != doc.bit_depth
                    || !step.value.is_finite()
                {
                    return Err("Invalid native mask identity, precision or value".into());
                }
                let valid = match step.kind.as_str() {
                    "blur" => (0.0..=64.0).contains(&step.value),
                    "levels" => (0.1..=5.0).contains(&step.value),
                    "fill" | "paint" | "invert" => (0.0..=255.0).contains(&step.value),
                    "density" => crate::engine::mask_parameter("density", f64::from(step.value)).is_ok()
                        && step.settings.is_null(),
                    "curves" | "adjust" | "gaussian" => {
                        crate::mask::commands::validate_settings(&step.kind, &step.settings)?;
                        true
                    }
                    _ => false,
                };
                if !valid {
                    return Err("Unsupported native mask step".into());
                }
            }
        }
    }
    metadata::validate_clipping(doc)?;
    metadata::validate_masks(doc, |r| roster.get(r))?;
    effects::validate_budget(doc, pool, check)?;
    validate_filters(doc, pool, check)?;
    if let Some(bounds) = doc.selection {
        validate_bounds(bounds)?;
    }
    if let Some(polygon) = &doc.selection_polygon {
        for vertex in polygon.iter() {
            check()?;
            if vertex.iter().any(|v| !v.is_finite() || v.abs() > 100000.) {
                return Err("Invalid native selection polygon".into());
            }
        }
    }
    for value in [
        doc.selection_coverage.as_ref(),
        doc.selection_previous.as_ref(),
    ]
    .into_iter()
    .flatten()
    {
        validate_coverage(value, check)?;
    }
    doc.visit_rasters(|r| {
        check()?;
        roster.get(r)?;
        if let Some(original) = r.retained.as_deref() {
            let original = RetainedMetadata(original, binding);
            let pixels = original.pixels();
            drop(crate::retained::pose::Pose::new(
                [pixels.width(), pixels.height()],
                pixels.depth(),
                Some(original),
                pool,
            )?);
        }
        Ok(())
    })?;
    check()
}

fn validate_bounds(bounds: [i32; 4]) -> Result<()> {
    if bounds.iter().any(|v| v.unsigned_abs() > 100000)
        || bounds[0] > bounds[2]
        || bounds[1] > bounds[3]
    {
        return Err("Invalid native selection bounds".into());
    }
    Ok(())
}
fn validate_coverage(value: &Coverage36, check: &dyn Fn() -> Result<()>) -> Result<()> {
    validate_bounds(value.bounds)?;
    if value.mask.depth != 8 || value.origin.iter().any(|v| v.unsigned_abs() > 100000) {
        return Err("Invalid native coverage precision or origin".into());
    }
    for contour in value.contours.iter() {
        check()?;
        for point in contour {
            if point.iter().any(|v| !v.is_finite() || v.abs() > 100000.) {
                return Err("Invalid native coverage contour".into());
            }
        }
    }
    Ok(())
}
pub(super) fn validate_filters(
    doc: &Document36,
    pool: &ResidentBudget,
    check: &dyn Fn() -> Result<()>,
) -> Result<()> {
    validate_effects(&doc.filters, pool, doc.bit_depth, true, check)
}

fn validate_effects(
    stack: &[Effect36],
    pool: &ResidentBudget,
    depth: u16,
    filters: bool,
    check: &dyn Fn() -> Result<()>,
) -> Result<()> {
    if stack.len() > 32 {
        return Err("Native effect/filter stack limit exceeded".into());
    }
    let mut bytes = 65536;
    for (at, effect) in stack.iter().enumerate() {
        check()?;
        if effect.id.is_empty()
            || stack[..at].iter().any(|e| e.id == effect.id)
            || (filters && (!effect.clips.is_empty() || !crate::filters::supports(&effect.kind)))
        {
            return Err("Invalid native effect/filter identity or kind".into());
        }
        crate::effects::validate_weight(effect.weight)?;
        crate::effects::regional::metadata_bytes(&effect.settings, 0, &mut bytes)?;
        let scratch = bytes
            .checked_mul(4)
            .ok_or("Native effect validation footprint overflow")?;
        let _scratch = MetadataCharge::reserve(pool, scratch, check)?;
        if filters {
            drop(crate::filters::settings(&effect.kind, &effect.settings)?);
        } else {
            drop(effects::normalize(
                &effect.kind,
                &effect.settings,
                pool,
                depth,
                check,
            )?);
        }
        for clip in &effect.clips {
            validate_coverage(&clip.coverage, check)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn changed(base: &NativeDocument, change: impl FnOnce(&mut Document36)) -> NativeDocument {
        let copied = copy_metadata(&base.core.metadata, base.budget(), &|| Ok(())).unwrap();
        let core = copied
            .rehome_unique(|mut metadata, _| {
                change(&mut metadata);
                Core::prepare(
                    metadata,
                    base.core.roster.clone(),
                    base.budget().clone(),
                    base.disk_budget().clone(),
                    &|| Ok(()),
                )
            })
            .unwrap();
        NativeDocument { core }
    }
    #[test]
    fn recovery_semantics_refuse_protection_exhausted_revisions_precision_and_bad_selection() {
        for depth in [8, 16] {
            let pool = ResidentBudget::new(4 * 1024 * 1024).unwrap();
            let doc = NativeDocument::new_blank(
                4,
                2,
                depth,
                &pool,
                &DiskBudget::new(1024, 4).unwrap(),
                &|| Ok(()),
            )
            .unwrap();
            doc.validate_editable(&|| Ok(())).unwrap();
            let key = doc.key();
            let baseline = pool.used();
            let variants: &[fn(&mut Document36)] = &[
                |d| d.read_only = true,
                |d| d.icc_profile = Some(vec![0, 127, 128, 255].into()),
                |d| d.revision = u64::MAX,
                |d| d.bit_depth = if d.bit_depth == 8 { 16 } else { 8 },
                |d| d.layers[0].parent = Some(d.layers[0].id.clone()),
                |d| d.layers[0].opacity = -0.5,
                |d| d.selection = Some([2, 0, 1, 2]),
            ];
            for change in variants {
                let altered = changed(&doc, *change);
                assert!(altered.validate_editable(&|| Ok(())).is_err());
                drop(altered);
                assert_eq!(pool.used(), baseline);
                assert_eq!(doc.key(), key);
            }
            assert!(doc.validate_editable(&|| Err("Cancelled".into())).is_err());
            assert_eq!(pool.used(), baseline);
        }
    }
}
