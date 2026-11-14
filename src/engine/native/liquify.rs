//! Append a local stroke while sharing its captured native selection source.
use super::*;
use crate::engine::scene::{Effect36, EffectClip36, MetadataPayload};

pub(super) fn prepare(edit: CapturedEdit, check: &dyn Fn() -> Result<()>) -> Result<PreparedDelta> {
    let checked = || {
        edit.cancellation.check()?;
        check()
    };
    checked()?;
    let descriptor = edit.descriptor().ok_or("Liquify capture missing")?;
    let command = descriptor
        .commands()
        .next()
        .ok_or("Liquify command missing")?;
    let c = command.parameters();
    if descriptor.commands().len() != 1
        || command.method() != Method::Effects
        || c["op"] != "liquify.stroke"
    {
        return Err("Liquify producer differs from its normalized command".into());
    }
    let base = edit.document();
    let original = &base.core.metadata;
    if original.read_only {
        return Err("This document is read-only".into());
    }
    let id = c["layer"].as_str().ok_or("Choose a target layer")?;
    super::patch::unlocked(original, id)?;
    let index = original
        .layers
        .iter()
        .position(|l| l.id == id)
        .ok_or("Unknown layer")?;
    let layer = &original.layers[index];
    if layer.kind == "adjustment" {
        return Err("Edit the adjustment effect settings or paint on its mask".into());
    }
    let origin = if layer.kind == "group" {
        [0, 0]
    } else {
        [layer.x, layer.y]
    };
    let selection = original.selection;
    let coverage = base
        .metadata()
        .selection_coverage()
        .filter(|m| selection == Some(m.bounds()));
    let polygon = base
        .metadata()
        .selection_polygon()
        .filter(|p| selection.is_some() && crate::selection::bounds(p).ok() == selection);
    // Reserve command/point translation and JSON growth before shared parsing.
    let mut bytes = 64 * 1024;
    crate::effects::regional::metadata_bytes(c, 0, &mut bytes)?;
    bytes = bytes
        .checked_add(polygon.map_or(0, |p| p.len() * 4096))
        .ok_or("Liquify metadata footprint overflow")?;
    let input = MetadataCharge::reserve(base.budget(), bytes, &checked)?;
    let mut stroke = crate::liquify::command::stroke(c, origin, selection, polygon)?;
    if coverage.is_some() {
        // Source36 omits inline coverage when a typed clip carries its pixels.
        stroke.as_object_mut().unwrap().remove("coverage");
    }
    // An explicitly empty mask is a no-op even when an effect ID was supplied.
    let empty = coverage.is_some_and(|m| m.bounds()[0] >= m.bounds()[2]);
    let target = if empty {
        None
    } else {
        crate::liquify::command::target(
            c,
            layer
                .effects
                .iter()
                .map(|e| (e.id.as_str(), e.kind.as_str(), e.enabled)),
        )?
    };
    let defaults;
    let old = if let Some(i) = target {
        &*layer.effects[i].settings
    } else {
        defaults = crate::liquify::defaults();
        &defaults
    };
    let mut bytes = 64 * 1024 + 32 * std::mem::size_of::<Effect36>();
    crate::effects::regional::metadata_bytes(old, 0, &mut bytes)?;
    crate::effects::regional::metadata_bytes(&stroke, 0, &mut bytes)?;
    // Normalization makes one settings copy; binding moves it. The per-node
    // envelope also covers the bounded parsed points used during validation.
    let growth = MetadataCharge::reserve(base.budget(), bytes, &checked)?;
    let copied =
        copy_metadata(original, base.budget(), &checked)?.retain_owner(&growth, &checked)?;
    let intent = edit.planned_intent(Intent::from_scopes(
        IntentKind::Capture,
        command.write_scopes(),
        base.budget(),
    )?)?;
    let core = copied.rehome_unique(|mut metadata, _| {
        if !empty {
            let settings = crate::liquify::command::append(old, stroke)?;
            let stroke_index = settings["strokes"]
                .as_array()
                .ok_or("Liquify stroke list missing")?
                .len()
                - 1;
            let settings = MetadataPayload::from(settings).bind(base.budget(), &checked)?;
            let new_clip = coverage
                .map(|coverage| -> Result<EffectClip36> {
                    let local = crate::retained::selected::effects::coverage(
                        base,
                        coverage,
                        [-origin[0], -origin[1]],
                        &checked,
                    )?;
                    // Existing-slot coverage preserves its retained original and pose.
                    let coverage =
                        super::batch::coverage_ref(base, &local, &mut Vec::new(), &checked)?;
                    Ok(EffectClip36 {
                        stroke: stroke_index as u32,
                        coverage,
                    })
                })
                .transpose()?;
            let effects = &mut metadata.layers[index].effects;
            let target = if let Some(target) = target {
                target
            } else {
                effects
                    .try_reserve_exact(1)
                    .map_err(|_| "Liquify effect allocation refused")?;
                effects.push(Effect36 {
                    id: crate::engine::id(),
                    kind: "liquify".into(),
                    enabled: true,
                    weight: 1.,
                    settings: MetadataPayload::from(serde_json::Value::Null),
                    clips: Vec::new(),
                });
                effects.len() - 1
            };
            let effect = &mut effects[target];
            effect.settings = settings;
            if let Some(clip) = new_clip {
                effect
                    .clips
                    .try_reserve_exact(1)
                    .map_err(|_| "Liquify clip allocation refused")?;
                effect.clips.push(clip);
            }
            let bytes = effect
                .clips
                .iter()
                .try_fold(0usize, |bytes, clip| -> Result<usize> {
                    checked()?;
                    let count = base
                        .roster()
                        .get(&clip.coverage.mask)?
                        .physical_tiles()
                        .count();
                    count
                        .checked_mul(256 * 256 * 4)
                        .and_then(|n| bytes.checked_add(n))
                        .ok_or_else(|| "Liquify selection footprint overflow".into())
                })?;
            if bytes > 128 * 1024 * 1024 {
                return Err("Liquify selection snapshots exceed the 128 MiB source budget".into());
            }
            super::metadata::validate_clipping(&metadata)?;
            super::effects::validate_budget(&metadata, base.budget(), &checked)?;
        }
        let roster = if empty || coverage.is_none() {
            base.core.roster.clone()
        } else {
            let mut locations = 0;
            metadata.remap_rasters(|_| {
                locations += 1;
                Ok(())
            })?;
            rebuild_from(
                &mut metadata,
                locations,
                base.budget(),
                base.disk_budget(),
                &checked,
                |reference| base.roster().get(reference).cloned(),
            )?
        };
        metadata.revision = metadata
            .revision
            .checked_add(1)
            .ok_or("Native revision overflow")?;
        checked()?;
        Core::prepare(
            metadata,
            roster,
            base.budget().clone(),
            base.disk_budget().clone(),
            &checked,
        )
    })?;
    drop(input);
    checked()?;
    Ok(PreparedDelta {
        base: base.key(),
        candidate: NativeDocument { core },
        cancellation: edit.cancellation,
        intent,
        outputs: vec![],
        admission: edit.admission,
    })
}
