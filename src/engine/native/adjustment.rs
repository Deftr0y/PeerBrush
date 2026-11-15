//! Atomic adjustment insertion, optional grouping and native selection-mask pixels.
use super::*;
use crate::engine::scene::{Effect36, Mask36, MaskStep36, MetadataPayload};
use crate::workspace::source_runtime::SourceSession;
use serde_json::Value;

pub(super) fn prepare(
    edit: CapturedEdit,
    session: &SourceSession,
    check: &dyn Fn() -> Result<()>,
) -> Result<PreparedDelta> {
    let base = edit.document().clone();
    let cancellation = edit.cancellation();
    let checked = || {
        cancellation.check()?;
        session.check()?;
        check()
    };
    checked()?;
    let descriptor = edit.descriptor().ok_or("Adjustment capture missing")?;
    let command = descriptor
        .commands()
        .next()
        .ok_or("Adjustment command missing")?;
    let c = command.parameters();
    if descriptor.commands().len() != 1
        || command.method() != Method::Effects
        || c["op"] != "adjustment.add"
    {
        return Err("Adjustment producer differs from its normalized command".into());
    }
    let original = &base.core.metadata;
    if original.read_only {
        return Err("This document is read-only".into());
    }
    if c["layers"].as_array().is_some_and(|ids| ids.len() > 100) {
        return Err("Choose 1–100 layers to copy".into());
    }
    let mut bytes = 64 * 1024;
    crate::effects::regional::metadata_bytes(c, 0, &mut bytes)?;
    for layer in &original.layers {
        bytes = bytes
            .checked_add(layer.id.len() + 128)
            .ok_or("Adjustment header footprint overflow")?;
    }
    let scratch = MetadataCharge::reserve(
        base.budget(),
        bytes
            .checked_mul(2)
            .ok_or("Adjustment footprint overflow")?,
        &checked,
    )?;
    let (kind, settings) = crate::adjustment::definition(c)?;
    let settings = MetadataPayload::from(settings).bind(base.budget(), &checked)?;
    let ids = crate::grouping::requested(c)?;
    let group = if ids.len() > 1 {
        Some(crate::grouping::Plan::new((&base).into(), &ids)?)
    } else {
        None
    };
    if original.layers.len() + 1 + usize::from(group.is_some()) > 100 {
        return Err("Initial version supports up to 100 layers".into());
    }
    let placement = if group.is_none() {
        Some(crate::adjustment::placement(
            (&base).into(),
            ids.first().map(String::as_str),
        )?)
    } else {
        None
    };
    let zero = Surface::zero(original.width, original.height, original.bit_depth)
        .map_err(|e| e.to_string())?;
    let ordinal =
        u32::try_from(base.roster().len() + 1).map_err(|_| "Adjustment source ordinal overflow")?;
    let reference = |id| RasterRef36 {
        id,
        width: zero.width(),
        height: zero.height(),
        depth: zero.depth(),
        retained: None,
    };
    let copied =
        copy_metadata(original, base.budget(), &checked)?.retain_owner(&scratch, &checked)?;
    let intent = edit.planned_intent(Intent::from_scopes(
        IntentKind::Capture,
        command.write_scopes(),
        base.budget(),
    )?)?;
    let mut outputs = Vec::new();
    outputs
        .try_reserve_exact(1)
        .map_err(|_| "Adjustment output allocation refused")?;
    let prepared = copied.rehome_unique(|mut metadata, _| {
        let placement = if let Some(plan) = group {
            let mut folder = layer("Color group", "group", reference(ordinal));
            folder.parent = plan.parent.map(|i| original.layers[i].id.clone());
            let id = folder.id.clone();
            for (index, child) in metadata.layers.iter_mut().enumerate() {
                if plan.root(index) {
                    child.parent = Some(id.clone());
                }
            }
            plan.apply(&mut metadata.layers, folder)?;
            let at = metadata
                .layers
                .iter()
                .position(|l| l.id == id)
                .ok_or("Adjustment folder missing")?;
            crate::adjustment::Placement {
                at: at + 1,
                parent: Some(id),
                clip_to: None,
            }
        } else {
            placement.ok_or("Adjustment placement missing")?
        };
        let mut adjustment = layer(
            c["name"].as_str().unwrap_or(kind),
            "adjustment",
            reference(ordinal),
        );
        adjustment.parent = placement.parent;
        adjustment.clip_to = placement.clip_to;
        adjustment.effects.push(Effect36 {
            id: crate::engine::id(),
            kind: kind.into(),
            enabled: true,
            weight: 1.,
            settings,
            clips: Vec::new(),
        });
        if original.selection.is_some() {
            adjustment.mask = Some(Mask36 {
                linked: true,
                enabled: true,
                steps: vec![
                    MaskStep36 {
                        id: crate::engine::id(),
                        kind: "fill".into(),
                        enabled: true,
                        weight: 1.,
                        value: 0.,
                        pixels: reference(ordinal),
                        settings: Value::Null.into(),
                    },
                    MaskStep36 {
                        id: crate::engine::id(),
                        kind: "paint".into(),
                        enabled: true,
                        weight: 1.,
                        value: 255.,
                        pixels: reference(ordinal + 1),
                        settings: Value::Null.into(),
                    },
                ],
            });
        }
        metadata
            .layers
            .try_reserve_exact(1)
            .map_err(|_| "Adjustment layer allocation refused")?;
        metadata.layers.insert(placement.at, adjustment);
        super::metadata::validate_clipping(&metadata)?;
        super::effects::validate_budget(&metadata, base.budget(), &checked)?;
        if original.selection.is_some() {
            outputs.push(crate::retained::transaction::stage(
                &base,
                session,
                &checked,
                |store, progress| mask(&base, store.clone(), &checked, progress),
            )?);
        }
        let resolve = |reference: &RasterRef36| -> Result<&Surface> {
            if reference.id == ordinal {
                Ok(&zero)
            } else if reference.id == ordinal + 1 {
                outputs
                    .first()
                    .map(StagedOutput::surface)
                    .ok_or_else(|| "Adjustment mask output missing".into())
            } else {
                base.roster().get(reference)
            }
        };
        super::metadata::validate_masks(&metadata, resolve)?;
        let mut locations = 0;
        metadata.remap_rasters(|_| {
            locations += 1;
            Ok(())
        })?;
        let roster = rebuild_from(
            &mut metadata,
            locations,
            base.budget(),
            base.disk_budget(),
            &checked,
            |reference| resolve(reference).cloned(),
        )?;
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
    });
    match prepared {
        Ok(core) => Ok(PreparedDelta {
            base: base.key(),
            candidate: NativeDocument { core },
            cancellation: edit.cancellation,
            intent,
            outputs,
            admission: edit.admission,
        }),
        Err(reason) => Err(
            match super::output::abort_outputs(outputs, session, &|| session.cleanup_check()) {
                Ok(()) => reason,
                Err(cleanup) => {
                    format!("{reason}; adjustment source retirement refused: {cleanup}")
                }
            },
        ),
    }
}

fn layer(name: &str, kind: &str, pixels: RasterRef36) -> Layer36 {
    Layer36 {
        id: crate::engine::id(),
        name: name.into(),
        kind: kind.into(),
        parent: None,
        clip_to: None,
        visible: true,
        locked: false,
        opacity: 1.,
        blend: "normal".into(),
        x: 0,
        y: 0,
        pixels,
        color: [233, 84, 32, 255],
        mask: None,
        effects: Vec::new(),
        psd_metadata: Vec::new().into(),
        source: None,
    }
}

fn mask(
    base: &NativeDocument,
    store: crate::large_document::Store,
    check: &dyn Fn() -> Result<()>,
    progress: &dyn Fn() -> crate::large_document::Result<()>,
) -> Result<Surface> {
    let selection = crate::edit_bounds::input::SelectionInput::new(base)?;
    let bounds = selection.bounds().ok_or("Adjustment selection missing")?;
    let metadata = base.metadata();
    let mut writer = crate::selection::NativeRows::new(
        metadata.width(),
        metadata.height(),
        metadata.depth(),
        store,
    )
    .map_err(|e| e.to_string())?;
    let bytes = metadata.width() as usize * 8;
    let mut row = ScratchBytes::new(base.budget(), bytes, bytes).map_err(|e| e.to_string())?;
    for y in 0..metadata.height() as i32 {
        check()?;
        row.fill(0);
        if y >= bounds[1] && y < bounds[3] {
            let view =
                selection.prepare([0, y, metadata.width() as i32, y + 1], &mut || check())?;
            for (x, pixel) in row.chunks_exact_mut(8).enumerate() {
                if x % 128 == 0 {
                    check()?;
                }
                let x = x as i32;
                if x < bounds[0]
                    || x >= bounds[2]
                    || selection.polygon().is_some_and(|p| {
                        !crate::selection::contains(p, x as f32 + 0.5, y as f32 + 0.5)
                    })
                {
                    continue;
                }
                let mut alpha = (f64::from(view.value(x, y)?) * 65535.).round() as u16;
                if metadata.depth() == 8 {
                    alpha = u16::from(crate::raster::project16(alpha)) * 257;
                }
                if alpha > 0 {
                    pixel[..6].fill(255);
                    pixel[6..].copy_from_slice(&alpha.to_be_bytes());
                }
            }
        }
        writer
            .row(y as u32, &row, &mut || progress())
            .map_err(|e| e.to_string())?;
    }
    writer.finish(&mut || progress()).map_err(|e| e.to_string())
}
