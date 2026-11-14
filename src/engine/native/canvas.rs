//! Canvas geometry moves metadata while retaining exact native raster owners.
use super::*;
use crate::{engine::scene::MetadataPayload, geometry::input::Frame};

pub(super) fn prepare(edit: CapturedEdit, check: &dyn Fn() -> Result<()>) -> Result<PreparedDelta> {
    let base = edit.document();
    let checked = || {
        edit.cancellation.check()?;
        check()
    };
    checked()?;
    let descriptor = edit.descriptor().ok_or("Canvas geometry capture missing")?;
    let command = descriptor
        .commands()
        .next()
        .ok_or("Canvas geometry command missing")?;
    if descriptor.commands().len() != 1
        || command.method() != Method::Geometry
        || !matches!(
            command.parameters()["op"].as_str(),
            Some("crop" | "canvas.resize")
        )
    {
        return Err("Canvas geometry differs from its captured command".into());
    }
    if base.metadata().read_only() {
        return Err("This PSD is read-only".into());
    }
    let frame = Frame::new(
        [base.metadata().width(), base.metadata().height()],
        command.parameters(),
        crate::raster::CanvasLimits::Native,
    )?;
    if frame.offset != [0, 0] && base.metadata().layers().any(|l| l.locked()) {
        return Err("Unlock layers before moving or resizing the whole image".into());
    }
    for layer in base.metadata().layers() {
        frame.origin(layer.origin())?;
    }
    let copied = copy_metadata(&base.core.metadata, base.budget(), &checked)?;
    let intent = edit.planned_intent(Intent::from_scopes(
        IntentKind::Capture,
        command.write_scopes(),
        base.budget(),
    )?)?;
    let core = copied.rehome_unique(|mut metadata, _| {
        for (index, original) in base.metadata().layers().enumerate() {
            checked()?;
            let world = matches!(original.kind(), "group" | "adjustment");
            let delta = if world { frame.offset } else { [0, 0] };
            let clips = original.effects().map(|e| e.clips().len()).sum::<usize>();
            let bytes = original.effects().len() * (std::mem::size_of::<EffectPatch>() + 512)
                + clips * (std::mem::size_of::<EffectClipPatch>() + 128)
                + 4096;
            let _patches = MetadataCharge::reserve(base.budget(), bytes, &checked)?;
            let patches =
                crate::retained::selected::effects::patches(base, original, delta, &checked)?;
            super::effect_patch::apply(base, &mut metadata, &patches, &mut vec![], &checked)?;
            let layer = &mut metadata.layers[index];
            layer.x += frame.offset[0];
            layer.y += frame.offset[1];
            for effect in &mut layer.effects {
                if effect.kind != "liquify" {
                    let offset = if world
                        || crate::effects::patterns::active(&effect.kind, &effect.settings)
                    {
                        frame.offset
                    } else {
                        [0, 0]
                    };
                    map_settings(
                        base,
                        &mut effect.settings,
                        &effect.kind,
                        frame,
                        offset,
                        &checked,
                    )?;
                }
            }
        }
        for filter in &mut metadata.filters {
            map_settings(
                base,
                &mut filter.settings,
                &filter.kind,
                frame,
                frame.offset,
                &checked,
            )?;
        }
        metadata.width = frame.width;
        metadata.height = frame.height;
        metadata.selection = None;
        metadata.selection_polygon = None;
        metadata.selection_coverage = None;
        metadata.selection_previous = None;
        let roster = rebuild_from(
            &mut metadata,
            base.roster().len(),
            base.budget(),
            base.disk_budget(),
            &checked,
            |r| base.roster().get(r).cloned(),
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
    checked()?;
    Ok(PreparedDelta {
        base: base.key(),
        candidate,
        cancellation: edit.cancellation,
        intent,
        outputs: vec![],
        admission: edit.admission,
    })
}

pub(super) fn map_settings(
    base: &NativeDocument,
    settings: &mut MetadataPayload<serde_json::Value>,
    kind: &str,
    frame: Frame,
    offset: [i32; 2],
    check: &dyn Fn() -> Result<()>,
) -> Result<()> {
    let extra = MetadataCharge::reserve(base.budget(), 4096, check)?;
    let mapped = base
        .copy_value_metadata(settings, check)?
        .retain_owner(&extra, check)?
        .map_unique(|mut settings, _| {
            crate::geometry::map_effect(
                kind,
                &mut settings,
                frame.scale,
                offset,
                frame.proportional,
            )?;
            Ok(settings)
        })?;
    *settings = MetadataPayload::copy_from(&*mapped, base.budget(), check)?;
    Ok(())
}
