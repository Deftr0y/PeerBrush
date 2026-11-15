//! Sampled/imported/refined coverage and its previous state publish together.
use super::*;
use crate::selection::{backed::CoverageSource, regional_refine, SelectionSource};

pub(super) fn prepare(
    edit: CapturedEdit,
    session: &SourceSession,
    check: &dyn Fn() -> Result<()>,
) -> Result<PreparedDelta> {
    let base = edit.document().clone();
    let command = edit
        .descriptor()
        .ok_or("Selection coverage descriptor missing")?
        .commands()
        .next()
        .ok_or("Selection coverage command missing")?;
    let token = command.token();
    let c = command.parameters();
    let importing = c["op"] == "selection.import";
    let updating = c["op"] == "selection.wand_update";
    let sampling = c["op"] == "selection" || updating;
    let replay = if updating || sampling && c["kind"] == "wand" {
        Some(super::super::super::wand::Replay::prepare(&edit, check)?)
    } else {
        None
    };
    // Updates combine with the original pre-click selection. Its coverage is
    // already the current document's previous selection, with current slot IDs.
    let has_current = if updating {
        replay
            .as_ref()
            .unwrap()
            .baseline()
            .metadata()
            .selection()
            .is_some()
    } else {
        base.metadata().selection().is_some()
    };
    let c = if updating {
        replay.as_ref().unwrap().command()
    } else {
        c
    };
    if !importing && !sampling && !has_current {
        return Err("Select an area before refining its edge".into());
    }
    let previous = if updating {
        base.metadata()
            .selection_previous()
            .map(|value| existing(&base, value, check))
            .transpose()?
    } else {
        match current(&base, session, check)? {
            Some(current) => Some(current),
            None => base
                .metadata()
                .selection_previous()
                .map(|value| existing(&base, value, check))
                .transpose()?,
        }
    };
    let mut batch = RasterBatch {
        rasters: vec![],
        layers: vec![],
        effects: vec![],
        selection: Some(SelectionPatch {
            bounds: None,
            polygon: None,
            current: None,
            previous,
        }),
    };
    let prepared = produce(
        &base,
        session,
        batch
            .selection
            .as_ref()
            .unwrap()
            .previous
            .as_ref()
            .filter(|_| has_current),
        c,
        check,
    );
    match prepared {
        Ok(Some(current)) => {
            let selection = batch.selection.as_mut().unwrap();
            selection.bounds = Some(current.bounds);
            selection.current = Some(current);
            let delta = apply_batch(edit, batch, session, check)?;
            if let Some(replay) = replay {
                match delta.with_wand(replay, check) {
                    Ok(delta) => Ok(delta),
                    Err(refused) => {
                        let reason = refused.reason().to_owned();
                        Err(cleanup(
                            reason,
                            refused.abort(session, &|| session.cleanup_check()),
                        ))
                    }
                }
            } else {
                Ok(delta)
            }
        }
        Ok(None) => {
            batch.abort(session, &|| session.cleanup_check())?;
            edit.unchanged(token, check)
        }
        Err(reason) => Err(cleanup(
            reason,
            batch.abort(session, &|| session.cleanup_check()),
        )),
    }
}

fn produce(
    base: &NativeDocument,
    session: &SourceSession,
    previous: Option<&CoveragePatch>,
    c: &serde_json::Value,
    check: &dyn Fn() -> Result<()>,
) -> Result<Option<CoveragePatch>> {
    let input = previous
        .map(|previous| -> Result<_> {
            let surface = match &previous.pixels {
                CoveragePixels::Existing(slot) => base.source(*slot)?,
                CoveragePixels::Output(output) => output.surface(),
            };
            Ok(SelectionSource::Coverage(
                CoverageSource::new(
                    surface.clone(),
                    previous.bounds,
                    previous.origin,
                    &previous.contours,
                    base.budget(),
                )
                .map_err(|e| e.to_string())?,
            ))
        })
        .transpose()?;
    let importing = c["op"] == "selection.import";
    let sampling = c["op"] == "selection";
    let (width, height) = (base.metadata().width(), base.metadata().height());
    let sampling_plan = sampling
        .then(|| crate::selection::sampling::Plan::capture(c, width, height))
        .transpose()?;
    let refinement = if !sampling && !importing {
        Some(regional_refine::SelectionPlan::capture(
            input.as_ref().ok_or("Selection disappeared")?,
            [width, height],
            c,
            check,
        )?)
    } else {
        None
    };
    let profiled = base.metadata().profile().is_some();
    let guide_bounds = sampling_plan
        .as_ref()
        .map(|plan| plan.guide_rect(profiled))
        .or_else(|| {
            refinement
                .as_ref()
                .and_then(|plan| plan.guide_rect(profiled))
        });
    let guide = if let Some(bounds) = guide_bounds {
        // Missing sample_merged defaults to true only for selection refinement.
        let target = if !sampling && c.get("sample_merged").is_none() || c["sample_merged"] == true
        {
            None
        } else {
            c["layer"].as_str()
        };
        Some((
            crate::render::native_preview::stage_guide(base, session, bounds, target, check)?,
            bounds,
        ))
    } else {
        None
    };
    let mut metadata = None;
    let result =
        crate::retained::transaction::stage_optional(base, session, check, |store, checked| {
            let coverage = if let Some(plan) = &sampling_plan {
                let (guide, bounds) = guide.as_ref().ok_or("Selection image source missing")?;
                Some(plan.prepare(
                    guide.surface(),
                    *bounds,
                    input.as_ref(),
                    store.clone(),
                    checked,
                )?)
            } else if let Some(plan) = &refinement {
                plan.prepare(
                    guide
                        .as_ref()
                        .map(|(guide, bounds)| (guide.surface(), *bounds)),
                    store.clone(),
                    checked,
                )?
            } else {
                Some(crate::selection::image_source::prepare(
                    c,
                    base.metadata().width(),
                    base.metadata().height(),
                    input.as_ref(),
                    store.clone(),
                    session,
                    checked,
                )?)
            };
            let Some(coverage) = coverage else {
                return Ok(None);
            };
            metadata = Some((
                coverage.bounds(),
                coverage.origin(),
                copy_admitted(&coverage.contours(), base.budget(), true, check)?,
            ));
            Ok(Some(coverage.mask().clone()))
        });
    let retired = guide.map_or(Ok(()), |(guide, _)| {
        guide.abort(session, &|| session.cleanup_check())
    });
    let output = match (result, retired) {
        (result, Ok(())) => result?,
        (Err(reason), Err(retired)) => return Err(cleanup(reason, Err(retired))),
        (Ok(None), Err(reason)) => return Err(reason),
        (Ok(Some(output)), Err(reason)) => {
            return Err(cleanup(
                reason,
                output.abort(session, &|| session.cleanup_check()),
            ))
        }
    };
    Ok(output.map(|output| {
        let (bounds, origin, contours) = metadata.unwrap();
        CoveragePatch {
            pixels: CoveragePixels::Output(output),
            bounds,
            origin,
            contours,
        }
    }))
}
