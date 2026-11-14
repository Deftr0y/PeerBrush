//! Selected pixels render at their original precision before atomic insertion.
use super::*;
use crate::{
    clipboard::native::{copy_area, stage_copy_in_document, Request},
    engine::scene::{SceneIdentity, SceneSnapshot},
    transform::native::Work,
    workspace::source_runtime::SourceSession,
};

pub(super) fn prepare(
    edit: CapturedEdit,
    session: &SourceSession,
    check: &dyn Fn() -> Result<()>,
) -> Result<PreparedDelta> {
    let base = edit.document().clone();
    let active = || edit.cancellation.check().and_then(|_| check());
    let checked = || session.check().and_then(|_| active());
    checked()?;
    let descriptor = edit
        .descriptor()
        .ok_or("Selected duplicate capture missing")?;
    let command = descriptor
        .commands()
        .next()
        .ok_or("Selected duplicate command missing")?;
    if descriptor.commands().len() != 1
        || command.method() != Method::Import
        || command.parameters()["op"] != "layer.duplicate_selection"
    {
        return Err("Selected duplicate differs from its captured command".into());
    }
    let c = command.parameters();
    let (target, mask) = crate::layer_clipboard::duplicate_selection_target((&base).into(), c)?;
    if base.metadata().selection().is_none() {
        return super::duplicate::prepare(edit, &|| session.check().and_then(|_| check()));
    }
    super::patch::unlocked(&base.core.metadata, target)?;
    let layers = &base.core.metadata.layers;
    if layers.len() >= 100 {
        return Err("Initial version supports up to 100 layers".into());
    }
    let at = layers
        .iter()
        .position(|l| l.id == target)
        .ok_or("The active layer no longer exists")?;
    let source = &layers[at];
    if !mask && source.kind == "adjustment" {
        return Err("Choose a paint layer or folder to duplicate selected pixels".into());
    }
    let [left, top, right, bottom] = copy_area(&base)?;
    let area = (right - left) as u64 * (bottom - top) as u64;
    // Allow one rendered sample and one simple coverage sample per output pixel.
    // Polygon edge evaluations still count against the same bounded allowance.
    let mut work = Work::new(128_000_000.max(2 * area))?;
    let insert = crate::layer_clipboard::duplicate_selection_index((&base).into(), at, mask);
    let bytes = (layers.len() + 1) * std::mem::size_of::<Layer36>()
        + source.name.len()
        + source.parent.as_ref().map_or(0, String::len)
        + source.clip_to.as_ref().map_or(0, String::len)
        + base.metadata().id().len()
        + 4096;
    let growth = MetadataCharge::reserve(base.budget(), bytes, &checked)?;
    let copied = copy_metadata(&base.core.metadata, base.budget(), &checked)?
        .retain_owner(&growth, &checked)?;
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
        .ok_or("Selected duplicate admission missing")?
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
    let scene = SceneSnapshot::from_native(
        SceneIdentity {
            project_id: String::new(),
            document_id: base.metadata().id().into(),
            revision: base.metadata().revision(),
        },
        base.clone(),
        vec![],
        None,
        None,
    )?;
    let (_, bounds, output) = stage_copy_in_document(
        &base,
        &scene,
        Request { target, mask, merged: false },
        session,
        &mut work,
        &active,
    )?
    .into_parts();
    drop(scene);
    let result = (|| {
        let surface = output.surface();
        // Selected copy clears all channels at zero alpha. The sealed
        // writer stores only nonzero tiles, so any stored tile has visible pixels.
        checked()?;
        if surface.physical_tiles().next().is_none() {
            return Err("The selection contains no visible pixels on this layer".into());
        }
        let ordinal = u32::try_from(base.roster().len())
            .map_err(|_| "Selected duplicate source ordinal overflow")?
            .checked_add(1)
            .ok_or("Selected duplicate source ordinal overflow")?;
        let mut layer = super::import::image_layer(
            &format!("{} {}", source.name, if mask { "mask selection" } else { "selection" }),
            source.parent.clone(),
            [bounds[0], bounds[1]],
            RasterRef36 {
                id: ordinal,
                width: surface.width(),
                height: surface.height(),
                depth: surface.depth(),
                retained: None,
            },
        );
        if !mask {
            layer.clip_to = source.clip_to.clone();
        }
        let core = copied.rehome_unique(|mut metadata, _| {
            metadata
                .layers
                .try_reserve_exact(1)
                .map_err(|_| "Selected duplicate header allocation refused")?;
            metadata.layers.insert(insert, layer);
            metadata.srgb_tagged |= metadata.layers.iter().any(|l| {
                l.effects
                    .iter()
                    .any(|e| crate::effects::patterns::active(&e.kind, &e.settings))
            });
            super::metadata::validate_clipping(&metadata)?;
            let roster = rebuild_from(
                &mut metadata,
                base.roster().len() + 1,
                base.budget(),
                base.disk_budget(),
                &checked,
                |r| {
                    if r.id == ordinal {
                        Ok(surface.clone())
                    } else {
                        base.roster().get(r).cloned()
                    }
                },
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
        checked()?;
        Ok(NativeDocument { core })
    })();
    match result {
        Ok(candidate) => Ok(PreparedDelta {
            base: base.key(),
            candidate,
            cancellation: edit.cancellation,
            intent,
            outputs: vec![output],
            admission: Some(admission),
        }),
        Err(reason) => Err(super::worker::cleanup(
            reason,
            output.abort(session, &|| session.cleanup_check()),
        )),
    }
}
