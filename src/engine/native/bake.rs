//! Explicit native prefix baking, accepted only after exact rendered comparison.
use super::*;
use crate::{
    bake::input::Plan, engine::scene::MaskStep36, workspace::source_runtime::SourceSession,
};
mod render;

pub(super) fn prepare(
    edit: CapturedEdit,
    session: &SourceSession,
    check: &dyn Fn() -> Result<()>,
) -> Result<PreparedDelta> {
    let base = edit.document().clone();
    let checked = || {
        edit.cancellation.check()?;
        session.check()?;
        check()
    };
    checked()?;
    let descriptor = edit.descriptor().ok_or("Bake capture missing")?;
    let command = descriptor.commands().next().ok_or("Bake command missing")?;
    if descriptor.commands().len() != 1
        || command.method() != Method::Bake
        || !matches!(
            command.parameters()["op"].as_str(),
            Some("effect.bake" | "mask.step.bake")
        )
    {
        return Err("Bake differs from its captured command".into());
    }
    let c = command.parameters();
    let target = c["layer"].as_str().ok_or("Choose a layer to bake")?;
    let index = base
        .metadata()
        .layers()
        .position(|l| l.id() == target)
        .ok_or("Layer no longer exists")?;
    let bytes = base.core.metadata.layers.len() * 64 + base.metadata().id().len() + 8192;
    let growth = MetadataCharge::reserve(base.budget(), bytes, &checked)?;
    let plan = Plan::new((&base).into(), index, c)?;
    crate::raster::CanvasLimits::Native.check(base.metadata().width(), base.metadata().height())?;
    if plan.active {
        crate::raster::CanvasLimits::Native.check(plan.size[0], plan.size[1])?;
    }
    let charge = Arc::new(
        base.budget()
            .reserve_bytes(
                std::mem::size_of::<StagedOutput>() + 4096,
                std::mem::size_of::<StagedOutput>() + 4096,
            )
            .map_err(|e| e.to_string())?,
    );
    let admission = edit
        .admission
        .as_ref()
        .ok_or("Bake admission missing")?
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
    let mut outputs = Vec::with_capacity(1);
    let result = (|| {
        if plan.active {
            outputs.push(if plan.mask {
                render::mask(&base, &plan, session, &checked)?
            } else {
                let prefix = modified(&base, &checked, |m| {
                    m.layers[index].effects.truncate(plan.at + 1);
                    Ok(())
                })?;
                render::color(&prefix, &plan, session, &checked)?
            });
        }
        let ordinal = base.roster().len() as u32 + 1;
        let copied = copy_metadata(&base.core.metadata, base.budget(), &checked)?
            .retain_owner(&growth, &checked)?;
        let core = copied.rehome_unique(|mut metadata, _| {
            let l = &mut metadata.layers[index];
            let reference = RasterRef36 {
                id: ordinal,
                width: plan.size[0],
                height: plan.size[1],
                depth: metadata.bit_depth,
                retained: None,
            };
            if plan.mask {
                let mask = l.mask.as_mut().ok_or("Layer has no mask")?;
                mask.steps.drain(..=plan.at);
                if plan.active {
                    mask.steps.insert(
                        0,
                        MaskStep36 {
                            id: crate::engine::id(),
                            kind: "paint".into(),
                            enabled: true,
                            weight: 1.,
                            value: 255.,
                            pixels: reference,
                            settings: serde_json::Value::Null.into(),
                        },
                    );
                }
            } else {
                l.effects.drain(..=plan.at);
                if plan.active {
                    l.pixels = reference;
                    l.source = None;
                    l.kind = "paint".into();
                    [l.x, l.y] = plan.origin;
                    let mut i = 0;
                    metadata.layers.retain(|_| {
                        let keep = i == index || !plan.tree.contains(&i);
                        i += 1;
                        keep
                    });
                }
            }
            let resolve = |r: &RasterRef36| {
                if r.id == ordinal {
                    outputs
                        .first()
                        .map(|o| o.surface().clone())
                        .ok_or_else(|| "Baked pixels missing".into())
                } else {
                    base.roster().get(r).cloned()
                }
            };
            super::metadata::validate_clipping(&metadata)?;
            let roster = rebuild_from(
                &mut metadata,
                base.roster().len() + 1,
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
        super::metadata::validate_masks(&candidate.core.metadata, |r| candidate.roster().get(r))?;
        if render::fingerprints(&base, Some(target), session, &checked)?
            != render::fingerprints(&candidate, Some(target), session, &checked)?
        {
            return Err(crate::bake::inexact());
        }
        let isolated_before = isolated(&base, target, &checked)?;
        let before = render::fingerprints(&isolated_before, None, session, &checked)?;
        drop(isolated_before);
        let isolated_after = isolated(&candidate, target, &checked)?;
        if before != render::fingerprints(&isolated_after, None, session, &checked)? {
            return Err(crate::bake::inexact());
        }
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

fn modified(
    base: &NativeDocument,
    check: &dyn Fn() -> Result<()>,
    change: impl FnOnce(&mut Document36) -> Result<()>,
) -> Result<NativeDocument> {
    let core = copy_metadata(&base.core.metadata, base.budget(), check)?.rehome_unique(
        |mut metadata, _| {
            change(&mut metadata)?;
            let roster = rebuild_from(
                &mut metadata,
                base.roster().len(),
                base.budget(),
                base.disk_budget(),
                check,
                |r| base.roster().get(r).cloned(),
            )?;
            Core::prepare(
                metadata,
                roster,
                base.budget().clone(),
                base.disk_budget().clone(),
                check,
            )
        },
    )?;
    Ok(NativeDocument { core })
}
fn isolated(
    base: &NativeDocument,
    target: &str,
    check: &dyn Fn() -> Result<()>,
) -> Result<NativeDocument> {
    let _charge = MetadataCharge::reserve(
        base.budget(),
        base.core.metadata.layers.len() * 32 + 4096,
        check,
    )?;
    let index = base
        .metadata()
        .layers()
        .position(|l| l.id() == target)
        .ok_or("Bake target disappeared")?;
    let tree = crate::bake::input::tree(base.into(), index);
    modified(base, check, |m| {
        let root = &mut m.layers[index];
        root.visible = true;
        root.parent = None;
        root.clip_to = None;
        root.opacity = 1.;
        root.blend = "normal".into();
        m.filters.clear();
        let mut i = 0;
        m.layers.retain(|_| {
            let keep = tree.contains(&i);
            i += 1;
            keep
        });
        Ok(())
    })
}
