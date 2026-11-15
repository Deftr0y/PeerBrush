//! Editable definitions and their exact native render enter one shared edit.
use super::*;
use crate::{
    engine::scene::{Content36, Source36},
    source::prepared::PreparedSource,
    source::text_position::NativeCarets,
};
#[cfg(test)]
#[path = "source_tests.rs"]
mod tests;

pub(super) fn prepare(
    edit: CapturedEdit,
    session: &SourceSession,
    check: &dyn Fn() -> Result<()>,
    carets: Option<&mut Option<NativeCarets>>,
) -> Result<PreparedDelta> {
    let base = edit.document().clone();
    let cancellation = edit.cancellation();
    let guarded = || {
        cancellation.check()?;
        session.check()?;
        check()
    };
    guarded()?;
    let command = edit
        .descriptor()
        .ok_or("Native source descriptor missing")?
        .commands()
        .next()
        .ok_or("Native source command missing")?;
    let operation = command.parameters()["op"].as_str().unwrap_or("");
    let updating = !matches!(operation, "source.add" | "path.create");
    if command.method() != Method::Source || handler(operation) != Some(Handler::Source) {
        return Err("Unsupported native editable-source producer".into());
    }
    let token = command.token();
    let layer = if updating {
        let id = command.parameters()["layer"]
            .as_str()
            .ok_or("Choose a target layer")?;
        let layer = base
            .metadata()
            .layers()
            .find(|layer| layer.id() == id)
            .ok_or("Unknown native layer")?;
        super::super::patch::unlocked(&base.core.metadata, id)?;
        Some(layer)
    } else {
        None
    };
    let mut source = if operation.starts_with("path.") {
        // A path has at most 128 anchors; reserve its possible vector growth
        // before editing, then carry that owner with the resulting definition.
        let extra = MetadataCharge::reserve(
            base.budget(),
            256 * std::mem::size_of::<crate::path::Anchor>() + 4096,
            &guarded,
        )?;
        if let Some(layer) = layer {
            let old = layer.source().ok_or("Choose an editable Pen path")?;
            if !matches!(old.definition(), Content36::Bezier { nodes, .. } if nodes.len() <= 128) {
                return Err("Choose an editable Pen path with at most 128 anchors".into());
            }
            let source: Admitted<crate::source::Source> =
                copy_admitted(old, base.budget(), false, &guarded)?;
            source
                .retain_owner(&extra, &guarded)?
                .map_unique(|mut source, _| {
                    crate::path::update_definition(
                        &mut source,
                        layer.origin(),
                        command.parameters(),
                    )?;
                    Ok(Source36::from_source(source))
                })?
        } else {
            let parameters = command.parameters();
            let frame = crate::path::creation_frame(
                [base.metadata().width(), base.metadata().height()],
                parameters,
            )?;
            if !parameters["nodes"]
                .as_array()
                .is_some_and(|nodes| nodes.len() <= 128)
            {
                return Err("Path creation needs at most 128 anchors in document pixels".into());
            }
            let nodes: Admitted<Vec<crate::path::Anchor>> =
                copy_admitted(&parameters["nodes"], base.budget(), false, &guarded)?;
            nodes
                .retain_owner(&extra, &guarded)?
                .map_unique(|nodes, _| {
                    Ok(Source36::from_source(crate::path::creation_source(
                        frame, parameters, nodes,
                    )?))
                })?
        }
    } else {
        // Command defaults belong to Source; strict Source36 wire fields stay strict.
        let source: Admitted<crate::source::Source> = copy_admitted(
            &command.parameters()["source"],
            base.budget(),
            false,
            &guarded,
        )?;
        source.map_unique(|source, _| Ok(Source36::from_source(source)))?
    };
    let mut frame = [source.width, source.height];
    if !updating {
        // Refuse bad placement before producing any full-frame source files.
        let placement = super::super::insertion::source_placement(&base, command.parameters(), &source)?;
        frame = placement.frame;
        if source.matrix != placement.matrix {
            source = source.map_unique(|mut source, _| {
                source.matrix = placement.matrix;
                Ok(source)
            })?;
        }
    }
    let mut origin = [0, 0];
    let mut resized = false;
    if let Some(layer) = layer {
        let old = layer
            .source()
            .ok_or("This layer has no editable text/vector source")?;
        frame = [layer.pixels().width(), layer.pixels().height()];
        origin = layer.origin();
        let fit_text = crate::source::fit_text_requested(command.parameters())?;
        if [old.width, old.height] != [source.width, source.height] || fit_text {
            if !matches!(old.definition(), Content36::Text { .. })
                || !matches!(source.definition(), Content36::Text { .. })
            {
                return Err("Only editable text fields can change source dimensions".into());
            }
            crate::source::prepared::validate_definition(&source, base.budget(), &mut || {
                guarded()
            })?;
            source = source.map_unique(|mut source, _| {
                (frame, origin, source.matrix) = crate::source::resized_text_frame(
                    source.matrix,
                    [source.width, source.height],
                    frame,
                    origin,
                )?;
                Ok(source)
            })?;
            if frame.iter().any(|n| *n == 0 || *n > 16000) {
                return Err("Editable native source frame exceeds admission".into());
            }
            crate::transform::work_budget(
                frame[0],
                frame[1],
                1 + layer.mask().map_or(0, |mask| mask.steps().len()),
            )?;
            resized = true;
        }
    }
    let mask_count = layer
        .filter(|_| resized)
        .and_then(|layer| layer.mask())
        .map_or(0, |mask| mask.steps().len());
    let (mut masks, _mask_charge) = crate::retained::planning::charged_planes(&base, mask_count)?;
    if let Some(layer) = layer.filter(|_| resized) {
        crate::retained::planning::reframe(&base, layer, frame, origin, &mut masks)?;
    }
    let mut bytes = (mask_count + 2)
        .checked_mul(32 * 1024)
        .ok_or("Source update packet overflow")?;
    if let Some(layer) = layer.filter(|layer| layer.origin() != origin) {
        for effect in layer.effects().filter(|effect| effect.kind() == "liquify") {
            bytes = effect
                .clips()
                .len()
                .checked_mul(std::mem::size_of::<EffectClipPatch>() + 128)
                .and_then(|n| n.checked_add(effect.id().len() + 4096))
                .and_then(|n| bytes.checked_add(n))
                .ok_or("Source effect packet overflow")?;
        }
    }
    let _packet = base
        .budget()
        .reserve_bytes(bytes, bytes)
        .map_err(|e| e.to_string())?;
    let mut batch = RasterBatch {
        rasters: Vec::new(),
        layers: Vec::new(),
        effects: Vec::new(),
        selection: None,
    };
    batch
        .rasters
        .try_reserve_exact(mask_count + 1)
        .map_err(|_| "Source raster packet allocation refused")?;
    batch
        .layers
        .try_reserve_exact(1)
        .map_err(|_| "Source layer packet allocation refused")?;
    if batch.rasters.capacity() * std::mem::size_of::<RasterPatch>()
        + batch.layers.capacity() * std::mem::size_of::<LayerPatch>()
        > _packet.bytes()
    {
        return Err("Source update packet exceeds admission".into());
    }
    if let Some(layer) = layer.filter(|layer| layer.origin() != origin) {
        batch.effects = crate::retained::selected::effects::patches(
            &base,
            layer,
            [layer.origin()[0] - origin[0], layer.origin()[1] - origin[1]],
            &guarded,
        )?;
    };
    let mut work = crate::transform::native::Work::new(128_000_000)?;
    let prepared = PreparedSource::load_with_carets(
        &source, source.matrix, base.budget(), &mut || guarded(), carets,
    )?;
    let output = crate::source::stage::render_captured(
        &prepared,
        frame,
        layer.map_or(base.metadata().depth(), |layer| layer.pixels().depth()),
        &base,
        session,
        &cancellation,
        &mut || guarded(),
    )?;
    drop(prepared);
    if let Some(layer) = layer {
        batch.rasters.push(RasterPatch {
            target: layer.pixels().slot(),
            output,
            retained: RetainedUpdate::Clear,
            layer_origin: None,
        });
        batch.layers.push(LayerPatch {
            target: layer.pixels().slot(),
            origin: Some(origin),
            source: SourceUpdate::Replace(source),
            materialize_fill: false,
        });
        for mask in &masks {
            match crate::retained::transaction::stage_plane(
                &base,
                mask,
                frame,
                session,
                &cancellation,
                &mut work,
                &guarded,
                &guarded,
            ) {
                Ok(patch) => batch.rasters.push(patch),
                Err(reason) => {
                    return Err(cleanup(
                        reason,
                        batch.abort(session, &|| session.cleanup_check()),
                    ))
                }
            }
        }
        return apply_batch(edit, batch, session, &guarded);
    }
    match edit.insert_source(token, output, source, &guarded) {
        Ok(delta) => Ok(delta),
        Err(refused) => {
            let reason = refused.reason().to_owned();
            Err(cleanup(
                reason,
                refused.abort(session, &|| session.cleanup_check()),
            ))
        }
    }
}
