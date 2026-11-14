//! Mask creation/refinement appends one opaque native step without changing selection.
use super::*;
#[cfg(test)]
#[path = "mask_tests.rs"]
pub(super) mod tests;
use crate::selection::{
    backed::CoverageSource, regional_masks, regional_masks::Source, SelectionSource,
};

fn path_coverage(
    base: &NativeDocument,
    session: &SourceSession,
    c: &serde_json::Value,
    check: &dyn Fn() -> Result<()>,
) -> Result<CoveragePatch> {
    let points = super::selection::path_points(base, c, check)?;
    let parameters = serde_json::json!({"kind":"polygon"});
    let geometry = crate::selection::geometry::Geometry::new(
        [base.metadata().width(), base.metadata().height()],
        &parameters,
        Some(&points),
    )?;
    let charge = base
        .budget()
        .reserve_bytes(points.len() * std::mem::size_of::<f32>(), 8192 * 4)
        .map_err(|e| e.to_string())?;
    let mut crossings = Vec::new();
    crossings
        .try_reserve_exact(points.len())
        .map_err(|_| "Path crossing allocation refused")?;
    if crossings.capacity() * std::mem::size_of::<f32>() > charge.bytes() {
        return Err("Path crossings exceed admission".into());
    }
    let feather = c["feather"]
        .as_f64()
        .filter(|v| v.is_finite())
        .unwrap_or(0.)
        .round()
        .clamp(0., 64.) as u32;
    let [width, height] = [base.metadata().width(), base.metadata().height()];
    let frame = crate::preview::regions::window(geometry.bounds(), feather as i32, width, height);
    super::selection::render(
        base,
        session,
        frame,
        feather,
        check,
        None,
        |y, row| {
            geometry.row_at(frame[0], frame[1] + y as i32, row, &mut crossings);
            Ok(())
        },
        |_, _| Ok(()),
    )
}
pub(super) fn prepare(
    edit: CapturedEdit,
    session: &SourceSession,
    check: &dyn Fn() -> Result<()>,
) -> Result<PreparedDelta> {
    if edit
        .descriptor()
        .and_then(|d| d.commands().next())
        .is_some_and(|c| c.parameters()["op"] == "mask.from_color")
    {
        return super::mask_color::prepare(edit, session, check);
    }
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
        .ok_or("Native mask descriptor missing")?
        .commands()
        .next()
        .ok_or("Native mask command missing")?;
    let token = command.token();
    let c = command.parameters();
    let path = c["op"] == "path.to_mask";
    let refine = c["op"] == "mask.refine";
    let adding = c["op"] == "mask.add";
    if !(path && command.method() == Method::Source
        || (c["op"] == "mask.from_selection" || refine || adding)
            && command.method() == Method::Mask)
    {
        return Err("Unsupported native mask producer".into());
    }
    if c.get("rect")
        .is_some_and(|r| !r.is_null() && crate::engine::rect(r).is_none())
    {
        return Err("Invalid rectangle coordinates".into());
    }
    if adding && base.metadata().selection().is_none() {
        return edit.metadata_command(token, &guarded);
    }
    let layer = super::super::insertion::mask_target(&base, c)?;
    if adding && layer.mask().is_some() {
        return Err("Layer already has a mask".into());
    }
    if refine && layer.mask().is_none() {
        return Err("Add a mask before refining it".into());
    }
    let mode = match if adding {
        "replace"
    } else {
        c[if path { "mask_mode" } else { "mode" }]
            .as_str()
            .unwrap_or("replace")
    } {
        "replace" => crate::selection::worker::Combine::Replace,
        "add" => crate::selection::worker::Combine::Add,
        "subtract" => crate::selection::worker::Combine::Subtract,
        "intersect" => crate::selection::worker::Combine::Intersect,
        _ => return Err("Mask mode must be replace, add, subtract or intersect".into()),
    };
    let _packet = base
        .budget()
        .reserve_bytes(4096, 4096)
        .map_err(|e| e.to_string())?;
    let metadata = base.metadata();
    let rectangle = metadata.selection().filter(|&bounds| {
        !refine && !path
            && !metadata.selection_coverage().is_some_and(|coverage| coverage.bounds() == bounds)
            && !metadata.selection_polygon().is_some_and(|points| {
                crate::selection::bounds(points).ok() == Some(bounds)
            })
    });
    let coverage = if refine || rectangle.is_some() {
        None
    } else if path {
        Some(path_coverage(&base, session, c, &guarded)?)
    } else {
        Some(
            super::selection::current(&base, session, &guarded)?
                .ok_or("Select an area before creating its mask")?,
        )
    };
    let result = (|| {
        let selected = coverage
            .as_ref()
            .map(|coverage| -> Result<_> {
                let surface = match &coverage.pixels {
                    CoveragePixels::Existing(slot) => base.source(*slot)?,
                    CoveragePixels::Output(output) => output.surface(),
                };
                Ok(SelectionSource::Coverage(
                    CoverageSource::new(
                        surface.clone(),
                        coverage.bounds,
                        coverage.origin,
                        &coverage.contours,
                        base.budget(),
                    )
                    .map_err(|e| e.to_string())?,
                ))
            })
            .transpose()?
            .or_else(|| rectangle.map(|bounds| SelectionSource::Geometry {
                bounds: crate::preview::regions::intersect(bounds, metadata.width(), metadata.height()),
                polygon: None,
            }));
        let mut source = regional_masks::native::Native::new(&base, layer, &guarded)?;
        let identity = source.identity_owner();
        let key = *source.source_key();
        let guide = if refine && c["edge"].as_f64().unwrap_or(0.) > 0. {
            let [x, y] = layer.origin();
            let frame = [x, y, x + layer.pixels().width() as i32, y + layer.pixels().height() as i32];
            Some(crate::render::native_preview::stage_layer_guide(
                &base, session, frame, layer.id(), &guarded,
            )?)
        } else {
            None
        };
        let result =
            crate::retained::transaction::stage(&base, session, &guarded, |store, checked| {
                regional_masks::prepare(
                    &mut source,
                    &identity,
                    layer.id(),
                    &key,
                    selected.as_ref(),
                    mode,
                    c,
                    guide.as_ref().map(StagedOutput::surface),
                    store.clone(),
                    checked,
                )
            });
        finish(result, guide, session)
    })();
    let temporary = coverage.and_then(|coverage| match coverage.pixels {
        CoveragePixels::Existing(_) => None,
        CoveragePixels::Output(output) => Some(output),
    });
    let output = finish(result, temporary, session)?;
    match edit.append_mask(token, output, &guarded) {
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
pub(super) fn finish(
    result: Result<StagedOutput>,
    temporary: Option<StagedOutput>,
    session: &SourceSession,
) -> Result<StagedOutput> {
    let retired = temporary.map_or(Ok(()), |output| {
        output.abort(session, &|| session.cleanup_check())
    });
    match (result, retired) {
        (Ok(output), Ok(())) => Ok(output),
        (Err(reason), retired) => Err(cleanup(reason, retired)),
        (Ok(output), Err(reason)) => Err(cleanup(
            reason,
            output.abort(session, &|| session.cleanup_check()),
        )),
    }
}
