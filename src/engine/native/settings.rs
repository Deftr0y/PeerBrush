//! Document-wide settings publish every converted plane in one history entry.
use super::*;
use crate::{
    engine::{
        document_settings::{global_light_angle, Settings, PATTERN_PRECISION},
        scene::{Effect36, MetadataPayload},
    },
    large_document::{Error, Store, TileSpec},
    raster::{surface::TilePatch, TILE},
    workspace::source_runtime::SourceSession,
};

pub(super) fn update_effect_settings(
    effect: &mut Effect36,
    pool: &ResidentBudget,
    check: &dyn Fn() -> Result<()>,
    change: impl FnOnce(&mut serde_json::Value),
) -> Result<()> {
    let copy: Admitted<serde_json::Value> = copy_admitted(effect.settings.value(), pool, true, check)?;
    copy.map_unique(|mut value, _| {
        change(&mut value);
        effect.settings = MetadataPayload::from(value).bind(pool, check)?;
        Ok(())
    })?;
    Ok(())
}

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
    let descriptor = edit
        .descriptor()
        .ok_or("Document settings capture missing")?;
    let command = descriptor
        .commands()
        .next()
        .ok_or("Document settings command missing")?;
    let operation = command.parameters()["op"].as_str();
    if descriptor.commands().len() != 1
        || command.method() != Method::Document
        || !matches!(operation, Some("document.settings" | "document.global_light"))
        || base.metadata().read_only()
    {
        return Err("Document settings require the editable captured document command".into());
    }
    let light = (operation == Some("document.global_light"))
        .then(|| global_light_angle(command.parameters())).transpose()?;
    let size = [base.metadata().width(), base.metadata().height()];
    let depth = base.metadata().depth();
    let settings = if light.is_some() {
        Settings { size, depth }
    } else {
        Settings::new(size, depth, command.parameters(), crate::raster::CanvasLimits::Native)?
    };
    if light.is_none() && settings.depth == 8 {
        for layer in &base.core.metadata.layers {
            for effect in &layer.effects {
                checked()?;
                if crate::effects::patterns::active(&effect.kind, &effect.settings) {
                    let encoded = effect.settings["bitmap_png"]
                        .as_str()
                        .ok_or("Bitmap pattern source missing")?;
                    let bitmap = crate::effects::patterns::PreparedBitmap::from_png(
                        encoded,
                        base.budget(),
                        &mut || checked(),
                    )?;
                    if bitmap.depth() == 16 {
                        return Err(PATTERN_PRECISION.into());
                    }
                }
            }
        }
    }
    let converting = settings.depth != base.metadata().depth();
    let count = if converting {
        base.core
            .metadata
            .layers
            .iter()
            .map(|l| 1 + l.mask.as_ref().map_or(0, |m| m.steps.len()))
            .sum()
    } else {
        0
    };
    let bytes = count * (std::mem::size_of::<StagedOutput>() + std::mem::size_of::<u32>()) + 4096;
    let charge = Arc::new(
        base.budget()
            .reserve_bytes(bytes, bytes)
            .map_err(|e| e.to_string())?,
    );
    let admission = edit
        .admission
        .as_ref()
        .ok_or("Document settings admission missing")?
        .with_scopes(
            &base,
            descriptor.shared_scopes(),
            charge,
            edit.admission
                .as_ref()
                .is_some_and(|a| a.is_transaction_step()),
        )?;
    let mut outputs = Vec::new();
    let mut ordinals = Vec::new();
    outputs
        .try_reserve_exact(count)
        .map_err(|_| "Precision output allocation refused")?;
    ordinals
        .try_reserve_exact(count)
        .map_err(|_| "Precision index allocation refused")?;
    if outputs.capacity() > count || ordinals.capacity() > count {
        return Err("Precision output allocation exceeds admission".into());
    }
    let prepared = (|| {
        if converting {
            for layer in &base.core.metadata.layers {
                for reference in std::iter::once(&layer.pixels).chain(
                    layer
                        .mask
                        .iter()
                        .flat_map(|m| m.steps.iter().map(|s| &s.pixels)),
                ) {
                    checked()?;
                    let source = base.roster().get(reference)?;
                    let output = crate::retained::transaction::stage(
                        &base,
                        session,
                        &checked,
                        |store, guard| {
                            convert(source, settings.depth, store, &mut || guard())
                                .map_err(|e| e.to_string())
                        },
                    )?;
                    outputs.push(output);
                    ordinals.push(reference.id);
                }
            }
        }
        let intent = edit.planned_intent(Intent::from_scopes(
            IntentKind::Capture,
            command.write_scopes(),
            base.budget(),
        )?)?;
        let copied = copy_metadata(&base.core.metadata, base.budget(), &checked)?;
        let core = copied.rehome_unique(|mut metadata, _| {
            if converting {
                for layer in &mut metadata.layers {
                    for reference in std::iter::once(&mut layer.pixels).chain(
                        layer
                            .mask
                            .iter_mut()
                            .flat_map(|m| m.steps.iter_mut().map(|s| &mut s.pixels)),
                    ) {
                        reference.depth = settings.depth;
                        reference.retained = None;
                    }
                }
            }
            if let Some(angle) = light {
                let previous = metadata.psd_global_angle;
                let resource = Some(angle.to_be_bytes());
                if previous != resource {
                    for effect in metadata.layers.iter_mut().flat_map(|layer| &mut layer.effects).chain(&mut metadata.filters) {
                        checked()?;
                        if let Some(light) = crate::effects::styles::GlobalLight::for_document(&effect.settings, previous) {
                            update_effect_settings(effect, base.budget(), &checked, |value| light.set_angle(value, angle))?;
                        }
                    }
                    metadata.psd_global_angle = resource;
                }
            } else {
                metadata.width = settings.size[0];
                metadata.height = settings.size[1];
                metadata.bit_depth = settings.depth;
                metadata.selection = None;
                metadata.selection_polygon = None;
                metadata.selection_coverage = None;
                metadata.selection_previous = None;
            }
            let roster = rebuild_from(
                &mut metadata,
                base.roster().len(),
                base.budget(),
                base.disk_budget(),
                &checked,
                |reference| {
                    if let Some(at) = ordinals.iter().position(|id| *id == reference.id) {
                        Ok(outputs[at].surface().clone())
                    } else {
                        base.roster().get(reference).cloned()
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
        Ok((NativeDocument { core }, intent))
    })();
    match prepared {
        Ok((candidate, intent)) => Ok(PreparedDelta {
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

/// Convert only stored tiles, preserving sparse holes, hidden RGB and padding.
/// The caller owns the provisional store and its publication/retirement.
pub(super) fn convert(
    source: &Surface,
    depth: u16,
    store: &Store,
    check: &mut dyn FnMut() -> crate::large_document::Result<()>,
) -> crate::large_document::Result<Surface> {
    let pool = store.resident_budget();
    let disk = store.disk_budget().ok_or(Error::Invalid(
        "Precision conversion needs its shared disk budget",
    ))?;
    let zero = Surface::zero(source.width(), source.height(), depth)?;
    let count = source.physical_tiles().count();
    let bytes = count * std::mem::size_of::<TilePatch>() + 128;
    let _charge = pool.reserve_bytes(bytes, bytes)?;
    let mut patches = Vec::new();
    patches
        .try_reserve_exact(count)
        .map_err(|_| Error::Budget)?;
    if patches.capacity() > count {
        return Err(Error::Budget);
    }
    let spec = TileSpec::new(TILE as u16, TILE as u16, depth)?;
    for tile in source.physical_tiles() {
        check()?;
        let input = source.prepare_tile(tile.coordinate(), check)?;
        let before = input
            .bytes()
            .ok_or(Error::Invalid("Stored precision source has no samples"))?;
        let mut after = ScratchBytes::new(&pool, spec.bytes(), spec.bytes())?;
        if source.depth() == depth {
            after.copy_from_slice(before);
        } else if depth == 16 {
            for (sample, bytes) in before.iter().zip(after.chunks_exact_mut(2)) {
                bytes.copy_from_slice(&(u16::from(*sample) * 257).to_be_bytes());
            }
        } else {
            for (bytes, sample) in before.chunks_exact(2).zip(after.iter_mut()) {
                *sample = crate::raster::project16(u16::from_be_bytes([bytes[0], bytes[1]]));
            }
        }
        let block = store.put(spec, &after, check)?;
        patches.push(TilePatch::stored(tile.coordinate(), store.clone(), block));
    }
    check()?;
    zero.with_tile_patches(&patches, &pool, &disk)
}
