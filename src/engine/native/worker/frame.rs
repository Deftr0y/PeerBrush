//! Adopt expanded paint and every linked source/placement as one guarded edit.
use super::*;
use crate::edit_bounds::{
    input::{InputFrame, RasterInput, WriteGuard},
    output::{stage_frame, BuiltTiles},
};

/// Resident retouch samples after frame expansion. Reuse that exact preparation
/// for a private read snapshot; its outputs must retire before the draw is sealed.
pub(crate) fn retouch_source(
    input: &RasterInput<'_>,
    frame: &InputFrame,
    guard: &WriteGuard<'_>,
    session: &SourceSession,
    check: &dyn Fn() -> Result<()>,
) -> Result<PreparedDelta> {
    guard.validate(input, Some(frame), &["clone", "heal"])?;
    let captured = guard.capture();
    let edit = CapturedEdit {
        base: captured.base.clone(),
        cancellation: captured.cancellation.clone(),
        admission: captured.admission.clone(),
    };
    let _charge = input
        .pool()
        .reserve_bytes(4096, 4096)
        .map_err(|e| e.to_string())?;
    let output = super::paint::seal(
        BuiltTiles::Draft(stage_frame(input, frame, session, &mut || check())?),
        check,
    )?
    .ok_or("Retouch frame source missing")?;
    let batch = RasterBatch {
        rasters: vec![RasterPatch {
            target: input.metadata().slot(),
            output,
            retained: RetainedUpdate::Keep,
            layer_origin: None,
        }],
        layers: vec![],
        effects: vec![],
        selection: None,
    };
    prepare(edit, input, frame, batch, false, session, check)
}

fn retained(input: &RasterInput<'_>, frame: &InputFrame, changed: bool) -> Result<RetainedUpdate> {
    if changed {
        return Ok(RetainedUpdate::Clear);
    }
    let Some(original) = input.metadata().retained() else {
        return Ok(RetainedUpdate::Keep);
    };
    let shift = frame.geometry().shift();
    let mut warps = Vec::new();
    warps
        .try_reserve_exact(original.warps().len())
        .map_err(|_| "Expanded retained placement allocation refused")?;
    warps.extend_from_slice(original.warps());
    Ok(RetainedUpdate::Replace {
        source: original.pixels().slot(),
        matrix: crate::retained::compose(
            [1., 0., 0., 1., shift[0] as f64, shift[1] as f64],
            original.matrix(),
        ),
        projective: original.projective(),
        warps,
    })
}

pub(super) fn prepare(
    edit: CapturedEdit,
    input: &RasterInput<'_>,
    frame: &InputFrame,
    mut batch: RasterBatch,
    changed: bool,
    session: &SourceSession,
    check: &dyn Fn() -> Result<()>,
) -> Result<PreparedDelta> {
    let cancellation = edit.cancellation();
    let checked = || {
        cancellation.check()?;
        session.check()?;
        check()?;
        cancellation.check()
    };
    let base = input.document();
    let layer = input.layer_metadata();
    // The draw output is already owned by batch before any further allocation
    // can fail, including reservation of the ancillary output headers.
    let assembled = (|| {
        checked()?;
        let count = 1 + layer.mask().map_or(0, |m| m.steps().len());
        let clips = layer.effects().map(|e| e.clips().len()).sum::<usize>();
        let bytes = count
            .checked_mul(32 * 1024 + std::mem::size_of::<RasterPatch>())
            .and_then(|n| n.checked_add(layer.effects().len() * 512))
            .and_then(|n| n.checked_add(clips * (std::mem::size_of::<EffectClipPatch>() + 128)))
            .and_then(|n| n.checked_add(4096))
            .ok_or("Expanded paint packet overflow")?;
        let charge = base
            .budget()
            .reserve_bytes(bytes, bytes)
            .map_err(|e| e.to_string())?;
        batch
            .rasters
            .try_reserve_exact(count - 1)
            .map_err(|_| "Expanded paint output allocation refused")?;
        let command = edit
            .descriptor()
            .ok_or("Expanded paint descriptor missing")?
            .commands()
            .next()
            .ok_or("Expanded paint command missing")?;
        let guard = WriteGuard::new(&edit, command, input, Some(frame))?;
        if batch.rasters.len() != 1 || batch.rasters[0].target != input.metadata().slot() {
            return Err("Expanded paint requires its exact staged draw target".into());
        }
        if !matches!(batch.rasters[0].retained, RetainedUpdate::ReplaceOutput { .. }) {
            batch.rasters[0].retained = retained(input, frame, changed)?;
        }
        let shift = frame.geometry().shift();
        batch.effects = crate::retained::selected::effects::patches(base, layer, shift, &checked)?;
        let mut materialize = layer.kind() == "fill" && !input.is_mask();
        if input.is_mask() {
            let color = RasterInput::layer(base, layer.id())?;
            let color_frame = frame.for_plane(&color)?;
            let original = retained(&color, &color_frame, layer.kind() == "fill")?;
            let built = if layer.kind() == "fill" {
                let visual = crate::fill::input::stage_visual_frame(
                    &color,
                    &color_frame,
                    &guard,
                    session,
                    &mut || checked(),
                )?;
                materialize = visual.materializes_fill();
                visual.into_parts().0
            } else {
                BuiltTiles::Draft(stage_frame(&color, &color_frame, session, &mut || {
                    checked()
                })?)
            };
            let output =
                super::paint::seal(built, &checked)?.ok_or("Expanded color output missing")?;
            batch.rasters.push(RasterPatch {
                target: color.metadata().slot(),
                output,
                retained: original,
                layer_origin: None,
            });
        }
        if let Some(mask) = layer.mask() {
            for step in mask.steps() {
                checked()?;
                if step.pixels().slot() == input.metadata().slot() {
                    continue;
                }
                let plane = RasterInput::mask_plane(base, layer.id(), step.id())?;
                let plane_frame = frame.for_plane(&plane)?;
                let original = retained(&plane, &plane_frame, false)?;
                let draft = stage_frame(&plane, &plane_frame, session, &mut || checked())?;
                let output = super::paint::seal(BuiltTiles::Draft(draft), &checked)?
                    .ok_or("Expanded mask output missing")?;
                batch.rasters.push(RasterPatch {
                    target: plane.metadata().slot(),
                    output,
                    retained: original,
                    layer_origin: None,
                });
            }
        }
        let source = if let Some(source) = layer.source() {
            let mut matrix = source.matrix;
            matrix[4] += shift[0] as f32;
            matrix[5] += shift[1] as f32;
            SourceUpdate::Matrix(matrix)
        } else {
            SourceUpdate::Keep
        };
        batch.layers.push(LayerPatch {
            target: layer.pixels().slot(),
            origin: Some(frame.geometry().new_origin()),
            source,
            materialize_fill: materialize,
        });
        checked()?;
        Ok(charge)
    })();
    let _charge = match assembled {
        Ok(charge) => charge,
        Err(reason) => {
            return Err(cleanup(
                reason,
                batch.abort(session, &|| session.cleanup_check()),
            ))
        }
    };
    let prepared = apply_batch(edit, batch, session, &checked)?;
    let candidate = prepared.preview();
    let validation =
        super::super::effects::validate_budget(&candidate.core.metadata, candidate.budget(), &checked)
            .and_then(|_| {
                super::super::metadata::validate_masks(&candidate.core.metadata, |reference| {
                    candidate.roster().get(reference)
                })
            });
    match validation {
        Ok(()) => Ok(prepared),
        Err(reason) => Err(cleanup(
            reason,
            prepared.abort(session, &|| session.cleanup_check()),
        )),
    }
}
