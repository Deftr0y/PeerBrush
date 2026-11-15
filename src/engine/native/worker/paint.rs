//! Nominal guarded paint producers. Every source and kind change is paired
//! before the shared transaction can enter live history.
use super::*;
use crate::edit_bounds::{
    input::{RasterInput, WriteGuard},
    output::BuiltTiles,
};

pub(super) fn seal(
    built: BuiltTiles<'_, '_>,
    check: &dyn Fn() -> Result<()>,
) -> Result<Option<StagedOutput>> {
    match built {
        BuiltTiles::Unchanged => Ok(None),
        BuiltTiles::Draft(draft) => match draft.seal(check) {
            Ok(output) => Ok(Some(output)),
            Err(refused) => {
                let reason = refused.reason().to_owned();
                Err(cleanup(reason, refused.abort()))
            }
        },
    }
}
pub(super) fn prepare(
    edit: CapturedEdit,
    session: &SourceSession,
    raster_damage: &mut Option<[i32; 4]>,
    check: &dyn Fn() -> Result<()>,
    check_active: &dyn Fn() -> Result<()>,
) -> Result<PreparedDelta> {
    let base = edit.document().clone();
    let _header_ticket = base
        .budget()
        .reserve_bytes(4096, 4096)
        .map_err(|e| e.to_string())?;
    let command = edit
        .descriptor()
        .ok_or("Native paint descriptor missing")?
        .commands()
        .next()
        .ok_or("Native paint command missing")?;
    let token = command.token();
    let parameters = command.parameters();
    let operation = parameters["op"]
        .as_str()
        .ok_or("Native paint operation missing")?;
    let target = parameters["layer"]
        .as_str()
        .ok_or("Choose a target layer")?;
    super::super::patch::unlocked(&base.core.metadata, target)?;
    let input = RasterInput::from_parameters(&base, parameters)?;
    if operation == "fill"
        && !input.is_mask()
        && input.layer_metadata().kind() == "fill"
        && base.metadata().selection().is_none()
    {
        return edit.metadata_command(token, check);
    }
    if input.layer_metadata().kind() == "group" && !input.is_mask() {
        if operation == "paint.clear_selection" {
            return clear_folder(edit, session, check);
        }
        if !matches!(operation, "paint.fill" | "fill") {
            return Err("Paint on a raster layer or its mask".into());
        }
        if base.metadata().layers().len() >= 100 {
            return Err("Initial version supports up to 100 layers".into());
        }
        let guard = WriteGuard::new(&edit, command, &input, None)?;
        let built = crate::fill::input::stage_child_fill(&input, &guard, session, &mut || check())?;
        let output = seal(built, check)?;
        drop(guard);
        return match output {
            None => edit.unchanged(token, check),
            Some(output) => match edit.insert_fill_child(token, output, check) {
                Ok(delta) => Ok(delta),
                Err(refused) => {
                    let reason = refused.reason().to_owned();
                    Err(cleanup(
                        reason,
                        refused.abort(session, &|| session.cleanup_check()),
                    ))
                }
            },
        };
    }
    let points = if matches!(operation, "paint" | "smudge" | "clone" | "heal") {
        Some(crate::brush::input::StrokePoints::from_command(
            command,
            base.budget(),
            &mut || check(),
        )?)
    } else {
        None
    };
    let frame = if let Some(points) = &points {
        input.stroke_frame(command, points, &mut || check())?
    } else if matches!(
        operation,
        "paint.fill" | "shape" | "gradient" | "fill" | "pixels.replace"
    ) {
        input.fill_frame(command)?
    } else {
        None
    };
    let mut raster_headers = Vec::new();
    raster_headers
        .try_reserve_exact(1)
        .map_err(|_| "Native fill output allocation refused")?;
    let mut layer_headers = Vec::new();
    layer_headers
        .try_reserve_exact(1)
        .map_err(|_| "Native fill metadata allocation refused")?;
    let guard = WriteGuard::new(&edit, command, &input, frame.as_ref())?;
    let mut materializes = false;
    let built = match operation {
        "image.patch" => super::image_patch::stage(&input, &guard, session, &mut || check())?,
        "shape" | "gradient" | "fill" | "adjust" | "pixels.replace" => {
            let result = super::primitive::stage(&input, frame.as_ref(), &guard, session,
                &mut || check(), check_active)?;
            materializes = result.materializes_fill();
            result.into_parts().0
        }
        "paint" | "smudge" | "clone" | "heal" => {
            let stroke = points
                .as_ref()
                .unwrap()
                .local(&input, frame.as_ref(), &mut || check())?;
            match operation {
                "paint" => crate::brush::input::stage_guarded(
                    &input,
                    &stroke,
                    frame.as_ref(),
                    &guard,
                    session,
                    &mut || check(),
                )?,
                "smudge" => crate::smudge::input::stage_guarded(
                    &input,
                    &stroke,
                    frame.as_ref(),
                    &guard,
                    session,
                    &mut || check(),
                )?,
                _ => crate::retouch::input::stage_guarded(
                    &input,
                    &stroke,
                    frame.as_ref(),
                    &guard,
                    session,
                    &mut || check(),
                )?,
            }
        }
        "paint.fill" | "paint.clear_selection" => {
            let result = if operation == "paint.fill" {
                crate::fill::input::stage_fill_guarded(
                    &input,
                    frame.as_ref(),
                    &guard,
                    session,
                    &mut || check(),
                )?
            } else {
                crate::fill::input::stage_clear_guarded(&input, &guard, session, &mut || check())?
            };
            materializes = result.materializes_fill();
            result.into_parts().0
        }
        _ => return Err("Unsupported guarded paint producer".into()),
    };
    let writes = guard.write_bounds();
    let changed = writes.is_some();
    let output = seal(built, check)?;
    drop(guard);
    let Some(output) = output else {
        return edit.unchanged(token, check);
    };
    let retained = if let Some(writes) = writes {
        let origin = frame.as_ref().map_or_else(|| input.origin(), |frame| frame.geometry().new_origin());
        match super::paint_mask_original::update(&input, output.surface(), origin, writes, session, check) {
            Ok(retained) => retained.unwrap_or(RetainedUpdate::Clear),
            Err(reason) => return Err(cleanup(reason, output.abort(session, &|| session.cleanup_check()))),
        }
    } else if materializes {
        RetainedUpdate::Clear
    } else {
        RetainedUpdate::Keep
    };
    let patch = RasterPatch {
        target: input.metadata().slot(),
        output,
        retained,
        layer_origin: None,
    };
    if let Some(frame) = &frame {
        raster_headers.push(patch);
        return super::frame::prepare(
            edit,
            &input,
            frame,
            RasterBatch {
                rasters: raster_headers,
                layers: layer_headers,
                effects: vec![],
                selection: None,
            },
            changed || materializes,
            session,
            check,
        );
    }
    if materializes {
        raster_headers.push(patch);
        layer_headers.push(LayerPatch {
            target: input.layer_metadata().pixels().slot(),
            origin: None,
            source: SourceUpdate::Keep,
            materialize_fill: true,
        });
        apply_batch(
            edit,
            RasterBatch {
                rasters: raster_headers,
                layers: layer_headers,
                effects: vec![],
                selection: None,
            },
            session,
            check,
        )
    } else {
        match edit.raster_patch(patch, check) {
            Ok(delta) => {
                *raster_damage = writes.filter(|_| local_display_write(&input));
                Ok(delta)
            }
            Err(refused) => {
                let reason = refused.reason().to_owned();
                Err(cleanup(
                    reason,
                    refused.abort(session, &|| session.cleanup_check()),
                ))
            }
        }
    }
}
fn local_display_write(input: &RasterInput<'_>) -> bool {
    // An alias can draw the same slot at a different document origin.
    let slot = input.metadata().slot();
    let owners = input.document().metadata().layers().flat_map(|layer| {
        std::iter::once(layer.pixels().slot()).chain(layer.mask().into_iter()
            .flat_map(|mask| mask.steps().map(|step| step.pixels().slot())))
    }).filter(|&owner| owner == slot).take(2).count();
    owners == 1 && (!input.is_mask() || input.layer_metadata().mask().is_some_and(|mask| {
        mask.steps().all(|step| !step.enabled() || step.weight() == 0.
            || matches!(step.kind(), "fill" | "paint" | "density" | "invert" | "levels"))
    }))
}
fn clear_folder(
    edit: CapturedEdit,
    session: &SourceSession,
    check: &dyn Fn() -> Result<()>,
) -> Result<PreparedDelta> {
    let base = edit.document().clone();
    let command = edit
        .descriptor()
        .ok_or("Native folder clear descriptor missing")?
        .commands()
        .next()
        .ok_or("Native folder clear command missing")?;
    let token = command.token();
    let target = command.parameters()["layer"]
        .as_str()
        .ok_or("Choose a folder")?;
    if base.metadata().selection().is_none() {
        return Err("Select pixels before clearing".into());
    }
    let count = base.metadata().layers().len();
    let bytes = count
        .checked_mul(std::mem::size_of::<RasterPatch>() + std::mem::size_of::<LayerPatch>() + 128)
        .and_then(|n| n.checked_add(4096))
        .ok_or("Native folder clear index overflow")?;
    let _ticket = base
        .budget()
        .reserve_bytes(bytes, bytes)
        .map_err(|e| e.to_string())?;
    let mut targets = Vec::new();
    targets
        .try_reserve_exact(count)
        .map_err(|_| "Native folder clear targets refused")?;
    for layer in base.metadata().layers() {
        let mut parent = Some(layer.id());
        let mut contained = false;
        for _ in 0..=16 {
            let Some(id) = parent else {
                break;
            };
            if id == target {
                contained = true;
                break;
            }
            parent = base
                .metadata()
                .layers()
                .find(|l| l.id() == id)
                .and_then(|l| l.parent());
        }
        if !contained {
            continue;
        }
        super::super::patch::unlocked(&base.core.metadata, layer.id())?;
        if layer.kind() == "group" {
            continue;
        }
        if !matches!(layer.kind(), "paint" | "fill") || layer.source().is_some() {
            return Err("Rasterize editable text/vector or adjustment layers before clearing selected color pixels".into());
        }
        targets.push(layer);
    }
    if targets.is_empty() {
        return Err("Folder has no raster pixels to clear".into());
    }
    let mut batch = RasterBatch {
        rasters: Vec::new(),
        layers: Vec::new(),
        effects: vec![],
        selection: None,
    };
    batch
        .rasters
        .try_reserve_exact(targets.len())
        .map_err(|_| "Native folder clear outputs refused")?;
    batch
        .layers
        .try_reserve_exact(targets.len())
        .map_err(|_| "Native folder clear kinds refused")?;
    let result = (|| {
        for layer in targets {
            check()?;
            session.check()?;
            let input = RasterInput::layer(&base, layer.id())?;
            let guard = WriteGuard::new(&edit, command, &input, None)?;
            let staged =
                crate::fill::input::stage_clear_guarded(&input, &guard, session, &mut || check())?;
            let materializes = staged.materializes_fill();
            let changed = guard.write_bounds().is_some();
            if let Some(output) = seal(staged.into_parts().0, check)? {
                batch.rasters.push(RasterPatch {
                    target: input.metadata().slot(),
                    output,
                    retained: if changed || materializes {
                        RetainedUpdate::Clear
                    } else {
                        RetainedUpdate::Keep
                    },
                    layer_origin: None,
                });
                if materializes {
                    batch.layers.push(LayerPatch {
                        target: layer.pixels().slot(),
                        origin: None,
                        source: SourceUpdate::Keep,
                        materialize_fill: true,
                    });
                }
            }
        }
        Ok(())
    })();
    if let Err(reason) = result {
        return Err(cleanup(
            reason,
            batch.abort(session, &|| session.cleanup_check()),
        ));
    }
    if batch.rasters.is_empty() {
        return edit.unchanged(token, check);
    }
    apply_batch(edit, batch, session, check)
}
