//! Typed clipboard insertion shares native sources and stages resident originals.
use super::*;
use crate::{clipboard::native::layers::LayerCopyPlan, workspace::source_runtime::SourceSession};

pub(super) fn prepare_resident(
    edit: CapturedEdit,
    clipboard: &crate::layer_clipboard::Layers,
    session: &SourceSession,
    check: &dyn Fn() -> Result<()>,
) -> Result<PreparedDelta> {
    let base = edit.document().clone();
    let cancellation = edit.cancellation.clone();
    let checked = || {
        cancellation.check()?;
        session.check()?;
        check()
    };
    let mut staged = Vec::new();
    let result = (|| {
        let source = super::super::scene::stage_resident_clipboard(
            clipboard,
            &base,
            session,
            &mut staged,
            &checked,
        )?;
        let plan = LayerCopyPlan::capture(
            &source,
            &clipboard.roots,
            &mut crate::transform::native::Work::new(4_000_000)?,
            &checked,
        )?;
        prepare(edit, &plan, session, &checked)
    })();
    let cleanup = || session.cleanup_check();
    let reason = match result {
        Ok(mut delta) => {
            if delta.outputs.try_reserve_exact(staged.len()).is_ok() {
                delta.outputs.append(&mut staged);
                return Ok(delta);
            }
            super::worker::cleanup(
                "Clipboard paste output allocation refused".into(),
                delta.abort(session, &cleanup),
            )
        }
        Err(reason) => reason,
    };
    Err(super::worker::cleanup(
        reason,
        super::output::abort_outputs(staged, session, &cleanup),
    ))
}

pub(super) fn prepare(
    edit: CapturedEdit,
    clipboard: &LayerCopyPlan,
    session: &SourceSession,
    check: &dyn Fn() -> Result<()>,
) -> Result<PreparedDelta> {
    let base = edit.document().clone();
    let source = clipboard.source();
    let checked = || {
        edit.cancellation.check()?;
        session.check()?;
        check()
    };
    checked()?;
    let descriptor = edit.descriptor().ok_or("Layer paste capture missing")?;
    let command = descriptor
        .commands()
        .next()
        .ok_or("Layer paste command missing")?;
    if descriptor.commands().len() != 1
        || command.method() != Method::Import
        || command.parameters()["op"] != "layer.paste"
        || base.metadata().read_only()
    {
        return Err("Layer paste requires its editable captured command".into());
    }
    if !source.budget().same_pool(base.budget())
        || !source.disk_budget().same_pool(base.disk_budget())
    {
        return Err("Layer clipboard belongs to another source service".into());
    }
    let target = command.parameters()["layer"].as_str().unwrap_or("");
    let parent_bytes = base
        .core
        .metadata
        .layers
        .iter()
        .map(|l| l.id.len().max(l.parent.as_ref().map_or(0, String::len)))
        .max()
        .unwrap_or(0);
    let _placement = MetadataCharge::reserve(base.budget(), parent_bytes + 4096, &checked)?;
    let plan = crate::clipboard::paste::Plan::layers(
        target,
        base.core.metadata.layers.len(),
        clipboard.members().len(),
        |i| {
            let l = &base.core.metadata.layers[i];
            crate::clipboard::paste::Layer {
                id: &l.id,
                parent: l.parent.as_deref(),
                kind: &l.kind,
                clip_to: l.clip_to.as_deref(),
                locked: l.locked,
            }
        },
    )?;
    let depth = base.metadata().depth().max(source.metadata().depth());
    let offset =
        u32::try_from(base.roster().len()).map_err(|_| "Clipboard source ordinal overflow")?;
    let folder_ordinal = offset
        .checked_add(
            u32::try_from(source.roster().len())
                .map_err(|_| "Clipboard source ordinal overflow")?,
        )
        .and_then(|n| n.checked_add(1))
        .ok_or("Clipboard source ordinal overflow")?;
    let count = base
        .core
        .metadata
        .layers
        .iter()
        .chain(
            clipboard
                .preorder()
                .iter()
                .map(|&i| &source.core.metadata.layers[i]),
        )
        .map(|l| 2 * (1 + l.mask.as_ref().map_or(0, |m| m.steps.len())))
        .sum::<usize>();
    let bytes = count * (std::mem::size_of::<StagedOutput>() + std::mem::size_of::<u32>()) + 4096;
    let charge = Arc::new(
        base.budget()
            .reserve_bytes(bytes, bytes)
            .map_err(|e| e.to_string())?,
    );
    let admission = edit
        .admission
        .as_ref()
        .ok_or("Layer paste admission missing")?
        .with_scopes(
            &base,
            descriptor.shared_scopes(),
            charge,
            edit.admission
                .as_ref()
                .is_some_and(|a| a.is_transaction_step()),
        )?;
    let mut bytes = 4096
        + (base.core.metadata.layers.len() + clipboard.members().len() + 1)
            * std::mem::size_of::<Layer36>();
    for &i in clipboard.preorder() {
        let l = &source.core.metadata.layers[i];
        bytes += 64 * (4 + l.effects.len() + l.mask.as_ref().map_or(0, |m| m.steps.len()));
        if clipboard.roots().contains(&i) {
            bytes += l.name.len() + 5;
        }
    }
    if let Some(index) = plan.wrap_base {
        let l = &base.core.metadata.layers[index];
        bytes += 2 * (l.name.len() + l.id.len() + l.blend.len() + 6);
    }
    bytes += (clipboard.roots().len() + 2) * plan.parent.as_ref().map_or(0, String::len);
    let growth = MetadataCharge::reserve(base.budget(), bytes, &checked)?;
    let copied = base
        .core
        .metadata
        .copy_runtime_with_layers_from(
            &source.core.metadata,
            clipboard.preorder(),
            base.budget(),
            &checked,
        )?
        .retain_owner(&growth, &checked)?;
    let intent = edit.planned_intent(Intent::from_scopes(
        IntentKind::Capture,
        command.write_scopes(),
        base.budget(),
    )?)?;
    let mut mapping: [Option<String>; 100] = std::array::from_fn(|_| None);
    for &i in clipboard.preorder() {
        mapping[i] = Some(crate::engine::id());
    }
    let mut outputs = Vec::new();
    let mut ordinals = Vec::new();
    outputs
        .try_reserve_exact(count)
        .map_err(|_| "Paste promotion allocation refused")?;
    ordinals
        .try_reserve_exact(count)
        .map_err(|_| "Paste source index allocation refused")?;
    if outputs.capacity() > count || ordinals.capacity() > count {
        return Err("Paste sources exceed admission".into());
    }
    let result = (|| {
        let core = copied.rehome_unique(|mut metadata, _| {
            let n = base.core.metadata.layers.len();
            let mut parent = plan.parent.clone();
            let folder = if let Some(index) = plan.wrap_base {
                let old = &base.core.metadata.layers[index];
                let mut folder = super::import::image_layer(
                    &format!("{} group", old.name),
                    None,
                    [0, 0],
                    RasterRef36 {
                        id: folder_ordinal,
                        width: metadata.width,
                        height: metadata.height,
                        depth,
                        retained: None,
                    },
                );
                folder.kind = "group".into();
                folder.blend = old.blend.clone();
                metadata.layers[index].blend = "normal".into();
                for layer in &mut metadata.layers[..n] {
                    if layer.parent == old.parent
                        && (layer.id == old.id || layer.clip_to.as_deref() == Some(&old.id))
                    {
                        layer.parent = Some(folder.id.clone());
                    }
                }
                parent = Some(folder.id.clone());
                Some(folder)
            } else {
                None
            };
            for (j, &i) in clipboard.preorder().iter().enumerate() {
                checked()?;
                let old = &source.core.metadata.layers[i];
                let layer = &mut metadata.layers[n + j];
                layer.id = mapping[i].as_ref().unwrap().clone();
                let remap = |id: &str| -> Result<String> {
                    source
                        .core
                        .metadata
                        .layers
                        .iter()
                        .position(|l| l.id == id)
                        .and_then(|i| mapping[i].as_ref())
                        .cloned()
                        .ok_or_else(|| "Clipboard relationship leaves its copied tree".into())
                };
                if clipboard.roots().contains(&i) {
                    layer.name = format!("{} copy", old.name);
                    layer.parent = parent.clone();
                } else {
                    layer.parent = Some(remap(
                        old.parent.as_deref().ok_or("Clipboard parent missing")?,
                    )?);
                }
                layer.clip_to = old.clip_to.as_deref().map(remap).transpose()?;
                super::duplicate::fresh_details(layer, base.budget(), &checked)?;
                for effect in &mut layer.effects {
                    if !crate::effects::styles::global_light_matches(&effect.settings, metadata.psd_global_angle) {
                        super::settings::update_effect_settings(effect, base.budget(), &checked, crate::effects::styles::detach_global_light)?;
                    }
                }
                layer.remap_rasters(|r| {
                    r.id =
                        r.id.checked_add(offset)
                            .ok_or("Clipboard source ordinal overflow")?;
                    Ok(())
                })?;
            }
            let input = |id: u32| -> Result<&Surface> {
                if id > offset {
                    source.roster().ordinal(id - offset)
                } else {
                    base.roster().ordinal(id)
                }
            };
            if depth == 16 {
                let mut promote = |reference: &mut RasterRef36| -> Result<()> {
                    if reference.depth == 16 {
                        return Ok(());
                    }
                    checked()?;
                    let pixels = input(reference.id)?;
                    outputs.push(crate::retained::transaction::stage(
                        &base,
                        session,
                        &checked,
                        |store, guard| {
                            super::settings::convert(pixels, 16, store, &mut || guard())
                                .map_err(|e| e.to_string())
                        },
                    )?);
                    ordinals.push(reference.id);
                    reference.depth = 16;
                    Ok(())
                };
                for layer in &mut metadata.layers {
                    for plane in std::iter::once(&mut layer.pixels).chain(
                        layer
                            .mask
                            .iter_mut()
                            .flat_map(|m| m.steps.iter_mut().map(|s| &mut s.pixels)),
                    ) {
                        promote(plane)?;
                        if let Some(original) = &mut plane.retained {
                            promote(&mut original.pixels)?;
                        }
                    }
                }
            }
            // The copied headers are a contiguous tail; rotate them into their
            // destination without allocating another header vector.
            let at = plan.index - usize::from(folder.is_some());
            let added = clipboard.preorder().len();
            metadata.layers[at..].rotate_right(added);
            if let Some(folder) = folder {
                metadata
                    .layers
                    .try_reserve_exact(1)
                    .map_err(|_| "Paste folder allocation refused")?;
                metadata.layers.insert(at, folder);
            }
            metadata.bit_depth = depth;
            metadata.selection = None;
            metadata.selection_polygon = None;
            metadata.selection_coverage = None;
            metadata.srgb_tagged |= metadata.layers.iter().any(|l| {
                l.effects
                    .iter()
                    .any(|e| crate::effects::patterns::active(&e.kind, &e.settings))
            });
            super::metadata::validate_clipping(&metadata)?;
            let zero =
                Surface::zero(metadata.width, metadata.height, depth).map_err(|e| e.to_string())?;
            let resolve = |reference: &RasterRef36| -> Result<Surface> {
                if reference.id == folder_ordinal {
                    return Ok(zero.clone());
                }
                if let Some(i) = ordinals.iter().position(|id| *id == reference.id) {
                    Ok(outputs[i].surface().clone())
                } else {
                    input(reference.id).cloned()
                }
            };
            // Shared source owners already carry their physical admission.
            // Summing references would charge copied layers' pixels again.
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
        checked()?;
        Ok(NativeDocument { core })
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
