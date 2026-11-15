//! Resample the complete image at native precision before one guarded adoption.
use super::*;
use crate::{
    engine::scene::{Coverage36, MetadataPayload},
    geometry::input::{scaled_size, Frame},
    raster::CanvasLimits,
    workspace::source_runtime::SourceSession,
};

const BASE_RESIZE_WORK: u64 = 128_000_000;

pub(super) fn prepare(
    edit: CapturedEdit,
    session: &SourceSession,
    check: &dyn Fn() -> Result<()>,
) -> Result<PreparedDelta> {
    let base = edit.document().clone();
    let active = || {
        edit.cancellation.check()?;
        check()
    };
    let checked = || {
        active()?;
        session.check()
    };
    let pinned = || {
        active()?;
        session.check_pinned_work()
    };
    checked()?;
    let descriptor = edit.descriptor().ok_or("Image resize capture missing")?;
    let command = descriptor
        .commands()
        .next()
        .ok_or("Image resize command missing")?;
    if descriptor.commands().len() != 1
        || command.method() != Method::Geometry
        || !matches!(
            command.parameters()["op"].as_str(),
            Some("image.resize" | "resize")
        )
    {
        return Err("Image resize differs from its captured command".into());
    }
    if base.metadata().read_only() {
        return Err("This PSD is read-only".into());
    }
    let frame = Frame::new(
        [base.metadata().width(), base.metadata().height()],
        command.parameters(),
        CanvasLimits::Native,
    )?;
    let changing = frame.scale != [1., 1.];
    let work_limit = BASE_RESIZE_WORK.max(u64::from(frame.width) * u64::from(frame.height));
    if changing && base.metadata().layers().any(|l| l.locked()) {
        return Err("Unlock layers before moving or resizing the whole image".into());
    }
    let count = base
        .core
        .metadata
        .layers
        .iter()
        .map(|l| {
            1 + l.mask.as_ref().map_or(0, |m| m.steps.len())
                + l.effects.iter().map(|e| e.clips.len()).sum::<usize>()
        })
        .sum::<usize>();
    let bytes = count * (std::mem::size_of::<StagedOutput>() + std::mem::size_of::<u32>()) + 4096;
    let charge = Arc::new(
        base.budget()
            .reserve_bytes(bytes, bytes)
            .map_err(|e| e.to_string())?,
    );
    let admission = edit
        .admission
        .as_ref()
        .ok_or("Image resize admission missing")?
        .with_scopes(
            &base,
            descriptor.shared_scopes(),
            charge,
            edit.admission
                .as_ref()
                .is_some_and(|a| a.is_transaction_step()),
        )?;
    let intent = edit.planned_intent(Intent::from_scopes(
        IntentKind::Capture,
        command.write_scopes(),
        base.budget(),
    )?)?;
    let copied = copy_metadata(&base.core.metadata, base.budget(), &checked)?;
    let mut outputs = Vec::new();
    let mut ordinals = Vec::new();
    outputs
        .try_reserve_exact(count)
        .map_err(|_| "Image resize output allocation refused")?;
    ordinals
        .try_reserve_exact(count)
        .map_err(|_| "Image resize ordinal allocation refused")?;
    if outputs.capacity() > count || ordinals.capacity() > count {
        return Err("Image resize exceeds its output admission".into());
    }
    let result = (|| {
        let core = copied.rehome_unique(|mut metadata, _| {
            let radius = (frame.scale[0] * frame.scale[1]).sqrt();
            let mut work = 0u64;
            // Validate every destination and mapped setting before staging pixels.
            if changing {
                for layer in &mut metadata.layers {
                    checked()?;
                    let [w, h] = scaled_size(
                        [layer.pixels.width, layer.pixels.height],
                        frame.scale,
                        CanvasLimits::Native,
                    )?;
                    let [x, y] = frame.origin([layer.x, layer.y])?;
                    if layer.source.is_some() || layer.kind == "fill" {
                        work += u64::from(w) * u64::from(h);
                    } else {
                        work += crate::geometry::native::resize_work(base.roster().get(&layer.pixels)?, [w, h], &pinned)?;
                    }
                    layer.x = x;
                    layer.y = y;
                    if let Some(mask) = &mut layer.mask {
                        for step in &mut mask.steps {
                            work += crate::geometry::native::resize_work(base.roster().get(&step.pixels)?, [w, h], &pinned)?;
                            if step.kind == "blur" {
                                if !frame.proportional {
                                    return Err(
                                        "Mask feather requires proportional image resizing".into(),
                                    );
                                }
                                step.value *= radius;
                                if !(0.0..=64.0).contains(&step.value) {
                                    return Err(
                                        "Resized mask feather exceeds supported radius".into()
                                    );
                                }
                            }
                            if step.kind == "gaussian" {
                                super::canvas::map_settings(
                                    &base,
                                    &mut step.settings,
                                    "blur",
                                    frame,
                                    [0, 0],
                                    &checked,
                                )?;
                            }
                        }
                    }
                    for effect in &mut layer.effects {
                        super::canvas::map_settings(
                            &base,
                            &mut effect.settings,
                            &effect.kind,
                            frame,
                            [0, 0],
                            &checked,
                        )?;
                        for clip in &effect.clips {
                            let size = scaled_size(
                                [clip.coverage.mask.width, clip.coverage.mask.height],
                                frame.scale,
                                CanvasLimits::Native,
                            )?;
                            work += crate::geometry::native::coverage_resize_work(
                                base.roster().get(&clip.coverage.mask)?, size, &pinned,
                            )?;
                        }
                    }
                    if work > work_limit {
                        let limit = if work_limit == BASE_RESIZE_WORK { "128-million".into() } else { work_limit.to_string() };
                        return Err(format!("Image resize exceeds the {limit}-sample work budget"));
                    }
                }
                for filter in &mut metadata.filters {
                    super::canvas::map_settings(
                        &base,
                        &mut filter.settings,
                        &filter.kind,
                        frame,
                        [0, 0],
                        &checked,
                    )?;
                }
                for layer in &mut metadata.layers {
                    checked()?;
                    let size = scaled_size(
                        [layer.pixels.width, layer.pixels.height],
                        frame.scale,
                        CanvasLimits::Native,
                    )?;
                    if let Some(source) = &mut layer.source {
                        source.matrix = crate::source::prepend_matrix(
                            [frame.scale[0], 0., 0., frame.scale[1], 0., 0.],
                            source.matrix,
                        );
                        let prepared = crate::source::prepared::PreparedSource::load(
                            source,
                            base.budget(),
                            &mut || checked(),
                        )?;
                        outputs.push(crate::source::stage::render_captured(
                            &prepared,
                            size,
                            metadata.bit_depth,
                            &base,
                            session,
                            &edit.cancellation,
                            &mut || checked(),
                        )?);
                        ordinals.push(layer.pixels.id);
                        layer.pixels.width = size[0];
                        layer.pixels.height = size[1];
                        layer.pixels.retained = None;
                    } else {
                        resample(
                            &base,
                            &mut layer.pixels,
                            size,
                            session,
                            &mut outputs,
                            &mut ordinals,
                            &active,
                            &pinned,
                        )?;
                    }
                    if let Some(mask) = &mut layer.mask {
                        for step in &mut mask.steps {
                            resample(
                                &base,
                                &mut step.pixels,
                                size,
                                session,
                                &mut outputs,
                                &mut ordinals,
                                &active,
                                &pinned,
                            )?;
                        }
                    }
                    for effect in &mut layer.effects {
                        for clip in &mut effect.clips {
                            resize_coverage(
                                &base,
                                &mut clip.coverage,
                                frame.scale,
                                session,
                                &mut outputs,
                                &mut ordinals,
                                &checked,
                            )?;
                        }
                    }
                }
                metadata.width = frame.width;
                metadata.height = frame.height;
                metadata.selection = None;
                metadata.selection_polygon = None;
                metadata.selection_coverage = None;
                metadata.selection_previous = None;
            }
            let resolve = |r: &RasterRef36| {
                if let Some(i) = ordinals.iter().position(|id| *id == r.id) {
                    Ok(outputs[i].surface().clone())
                } else {
                    base.roster().get(r).cloned()
                }
            };
            let roster = rebuild_from(
                &mut metadata,
                base.roster().len(),
                base.budget(),
                base.disk_budget(),
                &checked,
                resolve,
            )?;
            metadata.revision = metadata
                .revision
                .checked_add(1)
                .ok_or("Native revision overflow")?;
            Core::prepare(
                metadata,
                roster,
                base.budget().clone(),
                base.disk_budget().clone(),
                &checked,
            )
        })?;
        let candidate = NativeDocument { core };
        super::effects::validate_budget(&candidate.core.metadata, candidate.budget(), &checked)?;
        super::validation::validate_filters(&candidate.core.metadata, candidate.budget(), &checked)?;
        super::metadata::validate_masks(&candidate.core.metadata, |r| candidate.roster().get(r))?;
        checked()?;
        Ok(candidate)
    })();
    match result {
        Ok(candidate) => Ok(PreparedDelta {
            base: base.key(),
            candidate,
            cancellation: edit.cancellation,
            intent,
            outputs,
            admission: Some(admission),
        }),
        Err(reason) => Err(super::worker::cleanup(
            reason,
            super::output::abort_outputs(outputs, session, &|| session.cleanup_check()),
        )),
    }
}

fn resample(
    base: &NativeDocument,
    r: &mut RasterRef36,
    size: [u32; 2],
    session: &SourceSession,
    outputs: &mut Vec<StagedOutput>,
    ordinals: &mut Vec<u32>,
    check: &dyn Fn() -> Result<()>,
    pinned: &dyn Fn() -> Result<()>,
) -> Result<()> {
    if [r.width, r.height] == size {
        return Ok(());
    }
    let source = base.roster().get(r)?;
    outputs.push(crate::retained::transaction::stage(
        base,
        session,
        check,
        |store, guard| crate::geometry::native::resized(source, size, store, guard, pinned),
    )?);
    ordinals.push(r.id);
    r.width = size[0];
    r.height = size[1];
    r.retained = None;
    Ok(())
}

fn resize_coverage(
    base: &NativeDocument,
    c: &mut Coverage36,
    scale: [f32; 2],
    session: &SourceSession,
    outputs: &mut Vec<StagedOutput>,
    ordinals: &mut Vec<u32>,
    check: &dyn Fn() -> Result<()>,
) -> Result<()> {
    let size = scaled_size([c.mask.width, c.mask.height], scale, CanvasLimits::Native)?;
    let origin = std::array::from_fn::<_, 2, _>(|i| (c.origin[i] as f32 * scale[i]).round() as i32);
    outputs.push(crate::retained::transaction::stage(
        base,
        session,
        check,
        |store, guard| {
            let coverage = crate::geometry::native::coverage(
                base.roster().get(&c.mask)?,
                size,
                store.clone(),
                guard,
            )?;
            c.mask.width = size[0];
            c.mask.height = size[1];
            c.mask.retained = None;
            c.bounds = std::array::from_fn(|i| coverage.bounds()[i] + origin[i % 2]);
            c.origin = origin;
            let contours = base
                .copy_contour_metadata(coverage.contours(), check)?
                .map_unique(|mut contours, _| {
                    for line in &mut contours {
                        check()?;
                        for p in line {
                            for i in 0..2 {
                                p[i] -= (-origin[i]) as f32;
                            }
                        }
                    }
                    Ok(contours)
                })?;
            c.contours = MetadataPayload::copy_from(&*contours, base.budget(), check)?;
            // Only the returned surface may own the fragment while sealing;
            // a failed seal must be able to retire every provisional owner.
            Ok(coverage.mask().clone())
        },
    )?);
    ordinals.push(c.mask.id);
    Ok(())
}
