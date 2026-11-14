//! Raster-preserving structural edits over the same immutable normalized command.
use super::*;
use serde_json::Value;

impl CapturedEdit {
    pub(crate) fn metadata_command(
        self,
        token: CommandToken,
        check: &dyn Fn() -> Result<()>,
    ) -> Result<PreparedDelta> {
        self.cancellation.check()?;
        check()?;
        let descriptor = self
            .descriptor()
            .ok_or("Native metadata edit requires a normalized command")?;
        let command = descriptor
            .commands()
            .next()
            .ok_or("Missing native metadata command")?;
        if descriptor.commands().len() != 1
            || command.token() != token
            || !(command.method() == Method::Metadata
                || (command.method() == Method::Paint && command.parameters()["op"] == "fill")
                || (command.method() == Method::Effects
                    && matches!(
                        command.parameters()["op"].as_str(),
                        Some("effect.delete" | "effect.reorder")
                    ))
                || (command.method() == Method::Mask
                    && matches!(
                        command.parameters()["op"].as_str(),
                        Some("mask.add" | "mask.step.add")
                    ))
                || (command.method() == Method::Source
                    && command.parameters()["op"] == "source.rasterize"))
        {
            return Err("Native metadata edit differs from its normalized command".into());
        }
        let parameters = command.parameters();
        let operation = parameters["op"]
            .as_str()
            .ok_or("Missing native metadata operation")?;
        if !matches!(
            operation,
            "layer.update"
                | "fill"
                | "layer.add"
                | "layer.delete"
                | "layer.reorder"
                | "layer.clip"
                | "layer.parent"
                | "mask.remove"
                | "mask.toggle"
                | "mask.link"
                | "mask.add"
                | "mask.step.add"
                | "mask.step.update"
                | "mask.step.delete"
                | "mask.step.reorder"
                | "effect.delete"
                | "effect.reorder"
                | "source.rasterize"
                | "group.create_selected"
                | "layer.duplicate"
        ) {
            return Err("Native metadata operation needs its typed structural producer".into());
        }
        if operation == "layer.duplicate" {
            return super::duplicate::prepare(self, check);
        }
        let base = &self.base.core.metadata;
        let grouping = operation == "group.create_selected";
        let requested_bytes = if grouping {
            let ids = parameters["layers"].as_array();
            if ids.is_some_and(|ids| ids.len() > 100) {
                return Err("Choose 1–100 layers to copy".into());
            }
            ids.into_iter().flatten().try_fold(4096usize, |n, value| {
                n.checked_add(value.as_str().map_or(0, str::len) + std::mem::size_of::<String>())
                    .ok_or("Folder request footprint overflow")
            })?
        } else {
            0
        };
        // The containing folder ID may not appear in the request parameters.
        let parent_bytes = if grouping {
            base.layers
                .iter()
                .map(|layer| layer.id.len())
                .max()
                .unwrap_or(0)
        } else {
            0
        };
        let bytes = base
            .layers
            .len()
            .checked_mul(std::mem::size_of::<Layer36>() + if grouping { 128 } else { 32 })
            .and_then(|n| n.checked_add(requested_bytes))
            .and_then(|n| n.checked_add(parent_bytes))
            .and_then(|n| {
                n.checked_add(parameters.as_object().map_or(0, |o| {
                    o.values()
                        .filter_map(Value::as_str)
                        .map(str::len)
                        .sum::<usize>()
                }))
            })
            .and_then(|n| {
                n.checked_add(
                    parameters
                        .get("name")
                        .and_then(Value::as_str)
                        .map_or(0, str::len),
                )
            })
            .and_then(|n| n.checked_add(4096))
            .and_then(|n| {
                n.checked_add(if operation == "mask.step.add" {
                    32 * std::mem::size_of::<crate::engine::scene::MaskStep36>() + 256
                } else {
                    0
                })
            })
            .ok_or("Native structural metadata footprint overflow")?;
        let extra = MetadataCharge::reserve(self.base.budget(), bytes, check)?;
        let group = if grouping {
            let ids = crate::grouping::requested(parameters)?;
            Some(crate::grouping::Plan::new((&self.base).into(), &ids)?)
        } else {
            None
        };
        let copied = copy_metadata(base, self.base.budget(), check)?.retain_owner(&extra, check)?;
        let resident = self.base.budget().clone();
        let disk = self.base.disk_budget().clone();
        let intent = self.planned_intent(Intent::from_scopes(
            IntentKind::Capture,
            command.write_scopes(),
            self.base.budget(),
        )?)?;
        let core = copied.rehome_unique(|mut metadata, _| {
            let mut added = None;
            if operation == "layer.add" || grouping {
                if metadata.layers.len() >= 100 {
                    return Err("Initial version supports up to 100 layers".into());
                }
                let kind = if grouping {
                    "group"
                } else {
                    parameters["kind"].as_str().unwrap_or("paint")
                };
                if !matches!(kind, "paint" | "fill" | "group") {
                    return Err("Unsupported layer kind".into());
                }
                let parent = if let Some(group) = &group {
                    group.parent.map(|index| base.layers[index].id.as_str())
                } else {
                    parameters["parent"].as_str()
                };
                if let Some(parent) = parent {
                    if !metadata
                        .layers
                        .iter()
                        .any(|l| l.id == parent && l.kind == "group")
                    {
                        return Err("Parent must be an existing group".into());
                    }
                    super::patch::unlocked(&metadata, parent)?;
                }
                let surface = Surface::zero(metadata.width, metadata.height, metadata.bit_depth)
                    .map_err(|e| e.to_string())?;
                let ordinal = u32::try_from(self.base.roster().len() + 1)
                    .map_err(|_| "Native source ordinal overflow")?;
                let layer = Layer36 {
                    id: crate::engine::id(),
                    name: owned(parameters["name"].as_str().unwrap_or(if grouping {
                        "Folder"
                    } else {
                        "New layer"
                    }))?,
                    kind: owned(kind)?,
                    parent: parent.map(owned).transpose()?,
                    clip_to: None,
                    visible: true,
                    locked: false,
                    opacity: 1.,
                    blend: owned("normal")?,
                    x: 0,
                    y: 0,
                    pixels: RasterRef36 {
                        id: ordinal,
                        width: surface.width(),
                        height: surface.height(),
                        depth: surface.depth(),
                        retained: None,
                    },
                    color: if !grouping && parameters.get("color").is_some() {
                        crate::engine::color(parameters)
                    } else {
                        [233, 84, 32, 255]
                    },
                    mask: None,
                    effects: vec![],
                    psd_metadata: vec![].into(),
                    source: None,
                };
                if let Some(group) = &group {
                    for (index, child) in metadata.layers.iter_mut().enumerate() {
                        if group.root(index) {
                            child.parent = Some(owned(&layer.id)?);
                        }
                    }
                    group.apply(&mut metadata.layers, layer)?;
                } else {
                    metadata
                        .layers
                        .try_reserve_exact(1)
                        .map_err(|_| "Native layer allocation refused")?;
                    metadata.layers.insert(0, layer);
                }
                added = Some((ordinal, surface));
            } else {
                let target = parameters["layer"]
                    .as_str()
                    .ok_or("Choose a native layer")?;
                let index = metadata
                    .layers
                    .iter()
                    .position(|l| l.id == target)
                    .ok_or("Unknown native layer")?;
                let visibility_only = operation == "layer.update"
                    && parameters["visible"].is_boolean()
                    && parameters.as_object().is_some_and(|o| {
                        o.keys()
                            .all(|k| ["op", "layer", "visible"].contains(&k.as_str()))
                    });
                if !visibility_only {
                    if let Some(parent) = metadata.layers[index].parent.as_deref() {
                        super::patch::unlocked(&metadata, parent)?;
                    }
                    if metadata.layers[index].locked && operation != "layer.update" {
                        return Err("Layer is locked".into());
                    }
                }
                match operation {
                    "fill" => {
                        let layer = &mut metadata.layers[index];
                        if parameters
                            .get("mask")
                            .is_some_and(|v| v.as_bool() != Some(false))
                            || layer.kind != "fill"
                            || metadata.selection.is_some()
                            || layer.source.is_some()
                        {
                            return Err(
                                "Procedural recolor requires an unselected fill layer".into()
                            );
                        }
                        layer.color = crate::engine::color(parameters);
                    }
                    "layer.update" => {
                        let layer = &mut metadata.layers[index];
                        if !visibility_only
                            && layer.locked
                            && parameters.get("locked") != Some(&Value::Bool(false))
                        {
                            return Err("Unlock the layer before changing it".into());
                        }
                        if let Some(name) = parameters["name"].as_str() {
                            layer.name = owned(name)?;
                        }
                        if let Some(value) = parameters["visible"].as_bool() {
                            layer.visible = value;
                        }
                        if let Some(value) = parameters["locked"].as_bool() {
                            layer.locked = value;
                        }
                        if let Some(value) = parameters["opacity"].as_f64() {
                            layer.opacity = (value as f32).clamp(0., 1.);
                        }
                        if let Some(value) = parameters["blend"].as_str() {
                            if !crate::raster::layer_blends(&layer.kind)
                                .any(|(mode, _)| mode == value)
                            {
                                return Err("Unsupported blend mode".into());
                            }
                            layer.blend = owned(value)?;
                        }
                        if parameters.get("color").is_some() {
                            layer.color = crate::engine::color(parameters);
                        }
                        if parameters.get("fill_opacity").is_some() || parameters.get("blend_clipped_as_group").is_some()
                            || parameters.get("blend_if").is_some() || parameters.get("blend_channels").is_some()
                            || parameters.get("color_label").is_some()
                            || (!layer.locked && crate::psd::has_complete_layer_lock(layer.psd_metadata.value()))
                        {
                            let _scratch = resident.reserve_bytes(4096, 4096).map_err(|error| error.to_string())?;
                            let mut entries = layer.psd_metadata.value().clone();
                            if !layer.locked {
                                entries.retain(|entry| entry.key != "lspf");
                            }
                            crate::psd::fill_opacity::update(&mut entries, parameters)?;
                            crate::psd::clipping::update(&mut entries, parameters)?;
                            crate::psd::blend_ranges::update(&mut entries, parameters)?;
                            crate::psd::blend_channels::update(&mut entries, parameters)?;
                            crate::psd::update_layer_color_label(&mut entries, parameters)?;
                            if &entries != layer.psd_metadata.value() {
                                layer.psd_metadata = crate::engine::scene::MetadataPayload::from(entries)
                                    .bind(&resident, &|| { self.cancellation.check()?; check() })?;
                            }
                        }
                    }
                    "layer.delete" => {
                        let mut deleted = Vec::new();
                        deleted
                            .try_reserve_exact(metadata.layers.len())
                            .map_err(|_| "Native deletion allocation refused")?;
                        for layer in &metadata.layers {
                            deleted.push(descendant(&metadata, &layer.id, target)?);
                        }
                        if metadata
                            .layers
                            .iter()
                            .zip(&deleted)
                            .any(|(l, deleted)| *deleted && l.locked)
                        {
                            return Err(
                                "Unlock the selected layers and their children before deleting"
                                    .into(),
                            );
                        }
                        if metadata.layers.iter().zip(&deleted).any(|(l, removed)| {
                            !*removed
                                && l.clip_to.as_deref().is_some_and(|id| {
                                    metadata
                                        .layers
                                        .iter()
                                        .zip(&deleted)
                                        .any(|(l, removed)| *removed && l.id == id)
                                })
                        }) {
                            return Err("Release clipping before deleting its base".into());
                        }
                        let mut at = 0;
                        metadata.layers.retain(|_| {
                            let keep = !deleted[at];
                            at += 1;
                            keep
                        });
                    }
                    "layer.reorder" => {
                        let at = crate::engine::num(parameters, "index", 0.) as usize;
                        if at >= metadata.layers.len() {
                            return Err("Invalid layer index".into());
                        }
                        let layer = metadata.layers.remove(index);
                        metadata.layers.insert(at, layer);
                    }
                    "layer.clip" => {
                        metadata.layers[index].clip_to =
                            parameters["base"].as_str().map(owned).transpose()?
                    }
                    "layer.parent" => {
                        let parent = parameters["parent"].as_str();
                        let mut current = parent;
                        for depth in 0..=16 {
                            let Some(id) = current else { break };
                            if id == target {
                                return Err("A group cannot contain itself".into());
                            }
                            if depth == 16 {
                                return Err("Group nesting limit exceeded".into());
                            }
                            let node = metadata
                                .layers
                                .iter()
                                .find(|l| l.id == id && l.kind == "group")
                                .ok_or("Invalid parent group")?;
                            if node.locked {
                                return Err("Destination folder is locked".into());
                            }
                            current = node.parent.as_deref();
                        }
                        metadata.layers[index].parent = parent.map(owned).transpose()?;
                    }
                    "source.rasterize" => {
                        if metadata.layers[index].source.take().is_none() {
                            return Err("This layer has no editable source".into());
                        }
                    }
                    "effect.delete" | "effect.reorder" => {
                        let effects = &mut metadata.layers[index].effects;
                        let id = crate::engine::text(parameters, "effect", "");
                        let from = effects
                            .iter()
                            .position(|effect| effect.id == id)
                            .ok_or("Unknown effect")?;
                        if operation == "effect.delete" {
                            effects.remove(from);
                        } else {
                            let to = parameters["index"].as_u64().ok_or("Invalid effect index")?;
                            if to >= effects.len() as u64 {
                                return Err("Invalid effect index".into());
                            }
                            let effect = effects.remove(from);
                            effects.insert(to as usize, effect);
                        }
                    }
                    "mask.add" => {
                        if metadata.selection.is_some() {
                            return Err(
                                "Create selection masks through the native pixel worker".into()
                            );
                        }
                        let layer = &mut metadata.layers[index];
                        if layer.mask.is_some() {
                            return Err("Layer already has a mask".into());
                        }
                        let value = crate::engine::mask_parameter(
                            "fill",
                            crate::engine::num(parameters, "value", 255.),
                        )?;
                        let surface = Surface::zero(
                            layer.pixels.width,
                            layer.pixels.height,
                            layer.pixels.depth,
                        )
                        .map_err(|e| e.to_string())?;
                        let ordinal = u32::try_from(self.base.roster().len() + 1)
                            .map_err(|_| "Native mask ordinal overflow")?;
                        let mut steps = Vec::new();
                        steps
                            .try_reserve_exact(2)
                            .map_err(|_| "Native mask allocation refused")?;
                        for (kind, value) in [("fill", value), ("paint", 0.)] {
                            steps.push(crate::engine::scene::MaskStep36 {
                                id: crate::engine::id(),
                                kind: owned(kind)?,
                                enabled: true,
                                weight: 1.,
                                value,
                                pixels: RasterRef36 {
                                    id: ordinal,
                                    width: surface.width(),
                                    height: surface.height(),
                                    depth: surface.depth(),
                                    retained: None,
                                },
                                settings: Value::Null.into(),
                            });
                        }
                        layer.mask = Some(crate::engine::scene::Mask36 {
                            enabled: true,
                            linked: true,
                            steps,
                        });
                        // Both empty steps share a source; rebuilding assigns
                        // each reference its own canonical roster location.
                        added = Some((ordinal, surface));
                    }
                    "mask.remove" => metadata.layers[index].mask = None,
                    "mask.toggle" => {
                        let mask = metadata.layers[index]
                            .mask
                            .as_mut()
                            .ok_or("Layer has no mask")?;
                        mask.enabled = parameters["enabled"].as_bool().unwrap_or(!mask.enabled);
                    }
                    "mask.link" => {
                        let mask = metadata.layers[index]
                            .mask
                            .as_mut()
                            .ok_or("Layer has no mask")?;
                        mask.linked = parameters["linked"]
                            .as_bool()
                            .ok_or("Mask linked must be boolean")?;
                    }
                    "mask.step.add" => {
                        let layer = &mut metadata.layers[index];
                        let mask = layer.mask.as_mut().ok_or("Layer has no mask")?;
                        if mask.steps.len() >= 32 {
                            return Err("Initial mask stack supports up to 32 steps".into());
                        }
                        let definition = crate::mask::commands::NewStep::parse(parameters)?;
                        let settings = crate::engine::scene::MetadataPayload::copy_from(
                            definition.settings.as_ref(),
                            &resident,
                            &|| {
                                self.cancellation.check()?;
                                check()
                            },
                        )?;
                        let surface = Surface::zero(
                            layer.pixels.width,
                            layer.pixels.height,
                            layer.pixels.depth,
                        )
                        .map_err(|e| e.to_string())?;
                        let ordinal = u32::try_from(self.base.roster().len() + 1)
                            .map_err(|_| "Native mask ordinal overflow")?;
                        mask.steps
                            .try_reserve_exact(1)
                            .map_err(|_| "Native mask step allocation refused")?;
                        mask.steps.push(crate::engine::scene::MaskStep36 {
                            id: crate::engine::id(),
                            kind: owned(definition.kind)?,
                            enabled: true,
                            weight: definition.weight,
                            value: definition.value,
                            settings,
                            pixels: RasterRef36 {
                                id: ordinal,
                                width: surface.width(),
                                height: surface.height(),
                                depth: surface.depth(),
                                retained: None,
                            },
                        });
                        // Procedural mask effects never introduce a paint plane.
                        validate_masks(&metadata, |reference| self.base.roster().get(reference))?;
                        added = Some((ordinal, surface));
                    }
                    "mask.step.update" | "mask.step.delete" | "mask.step.reorder" => {
                        let mask = metadata.layers[index]
                            .mask
                            .as_mut()
                            .ok_or("Layer has no mask")?;
                        let id = crate::engine::text(parameters, "step", "");
                        if operation == "mask.step.delete" {
                            mask.steps.retain(|step| step.id != id);
                        } else {
                            let at = mask
                                .steps
                                .iter()
                                .position(|step| step.id == id)
                                .ok_or("Unknown mask step")?;
                            if operation == "mask.step.reorder" {
                                let to = crate::engine::num(parameters, "index", 0.) as usize;
                                if to >= mask.steps.len() {
                                    return Err("Invalid stack index".into());
                                }
                                let step = mask.steps.remove(at);
                                mask.steps.insert(to, step);
                            } else {
                                let step = &mut mask.steps[at];
                                let update = crate::mask::commands::StepUpdate::parse(
                                    &step.kind, parameters,
                                )?;
                                if let Some(enabled) = update.enabled {
                                    step.enabled = enabled;
                                }
                                if let Some(weight) = update.weight {
                                    step.weight = weight;
                                }
                                if let Some(value) = update.value {
                                    step.value = value;
                                }
                                if let Some(settings) = update.settings {
                                    if step.settings.value() != settings {
                                        step.settings =
                                            crate::engine::scene::MetadataPayload::copy_from(
                                                settings,
                                                &resident,
                                                &|| {
                                                    self.cancellation.check()?;
                                                    check()
                                                },
                                            )?;
                                    }
                                }
                            }
                        }
                        validate_masks(&metadata, |reference| self.base.roster().get(reference))?;
                    }
                    _ => return Err("Unsupported native structural operation".into()),
                }
            }
            validate_clipping(&metadata)?;
            let roster = rebuild_from(
                &mut metadata,
                self.base.roster().len()
                    + if operation == "mask.add" {
                        2
                    } else {
                        usize::from(added.is_some())
                    },
                &resident,
                &disk,
                check,
                |reference| match &added {
                    Some((id, surface)) if *id == reference.id => Ok(surface.clone()),
                    _ => self.base.roster().get(reference).cloned(),
                },
            )?;
            metadata.revision = metadata
                .revision
                .checked_add(1)
                .ok_or("Native revision overflow")?;
            self.cancellation.check()?;
            check()?;
            Core::prepare(metadata, roster, resident, disk, check)
        })?;
        Ok(PreparedDelta {
            base: self.base.key(),
            candidate: NativeDocument { core },
            cancellation: self.cancellation,
            intent,
            outputs: vec![],
            admission: self.admission,
        })
    }
}
fn owned(value: &str) -> Result<String> {
    let mut copy = String::new();
    copy.try_reserve_exact(value.len())
        .map_err(|_| "Native metadata string allocation refused")?;
    copy.push_str(value);
    Ok(copy)
}
pub(super) fn validate_masks<'a>(
    metadata: &Document36,
    source: impl Fn(&RasterRef36) -> Result<&'a Surface>,
) -> Result<()> {
    let mut bytes = 0u64;
    let mut native_bytes = 0u64;
    let mut all_samples_fit = true;
    for layer in &metadata.layers {
        let Some(mask) = &layer.mask else { continue };
        if !mask.steps.iter().any(|step| {
            crate::mask::needs_cache_step(&step.kind, step.enabled, step.weight, step.value)
        }) {
            continue;
        }
        all_samples_fit &= crate::mask::region::native_sample_fits(
            layer.pixels.width,
            layer.pixels.height,
            metadata.bit_depth,
            mask.steps.iter().map(|step| crate::mask::region::Step {
                kind: &step.kind,
                enabled: step.enabled,
                weight: step.weight,
                value: step.value,
                settings: step.settings.value(),
            }),
        )?;
        let pixels = u64::from(layer.pixels.width) * u64::from(layer.pixels.height);
        bytes = bytes.checked_add(pixels).ok_or("Mask budget overflow")?;
        if metadata.bit_depth == 16 {
            let mut uniform = true;
            for step in &mask.steps {
                if step.enabled
                    && step.weight > 0.
                    && step.kind == "paint"
                    && !source(&step.pixels)?.is_known_zero()
                {
                    uniform = false;
                }
            }
            native_bytes = native_bytes
                .checked_add(if uniform { 0 } else { pixels * 2 })
                .ok_or("16-bit mask budget overflow")?;
        }
    }
    if all_samples_fit {
        return Ok(());
    }
    // shortcut: retain the previous admission if one sample's halo can exceed
    // regional scratch, until the renderer can stage individual passes.
    crate::mask::validate_cache_bytes(bytes)?;
    if native_bytes > crate::depth16::MASK_BUDGET {
        return Err("Native16 spatial masks exceed the128 MiB mask budget".into());
    }
    Ok(())
}
fn descendant(metadata: &Document36, child: &str, parent: &str) -> Result<bool> {
    let mut current = Some(child);
    for _ in 0..=16 {
        let Some(id) = current else { return Ok(false) };
        if id == parent {
            return Ok(true);
        }
        current = metadata
            .layers
            .iter()
            .find(|l| l.id == id)
            .ok_or("Invalid native parent")?
            .parent
            .as_deref();
    }
    Err("Native group nesting limit exceeded".into())
}
pub(super) fn validate_clipping(metadata: &Document36) -> Result<()> {
    for (i, layer) in metadata.layers.iter().enumerate() {
        if crate::psd::blend_channels::excluded(&layer.psd_metadata) != 0 {
            crate::psd::blend_channels::validate_layer(
                &layer.psd_metadata, &layer.kind, &layer.blend,
                layer.effects.iter().map(|effect| effect.kind.as_str()),
                !crate::psd::blend_channels::supports_clip_base(&layer.kind, layer.effects.iter().map(|effect| effect.kind.as_str()),
                    crate::psd::clipping::grouped(&layer.psd_metadata))
                    && metadata.layers.iter().any(|node| node.clip_to.as_deref() == Some(&layer.id)),
            )?;
        }
        crate::psd::blend_ranges::validate_layer(
            &layer.psd_metadata, &layer.kind, &layer.blend,
            layer.effects.iter().map(|effect| effect.enabled), layer.clip_to.is_some(),
            crate::psd::blend_ranges::bytes(&layer.psd_metadata).is_some()
                && !crate::psd::blend_ranges::supports_clip_base(&layer.kind, &layer.blend, !layer.effects.is_empty(),
                    crate::psd::clipping::grouped(&layer.psd_metadata), crate::psd::fill_opacity::value(&layer.psd_metadata))
                && metadata.layers.iter().any(|node| node.clip_to.as_deref() == Some(&layer.id)),
        )?;
        crate::psd::fill_opacity::validate(
            crate::psd::fill_opacity::value(&layer.psd_metadata), &layer.kind, &layer.blend,
            layer.effects.iter().map(|effect| effect.enabled),
        )?;
        if !crate::psd::clipping::grouped(&layer.psd_metadata)
            && metadata.layers.iter().any(|node| node.clip_to.as_deref() == Some(&layer.id))
        {
            crate::psd::clipping::validate_base(&layer.psd_metadata, &layer.kind, &layer.blend, !layer.effects.is_empty())?;
        }
        if layer.blend == "pass_through" {
            if layer.kind != "group" {
                return Err("Pass Through is available only for folders".into());
            }
            if layer.effects.iter().any(|effect| effect.enabled) {
                return Err(
                    "Choose an isolated folder blend before enabling folder effects".into(),
                );
            }
            if layer.clip_to.is_some()
                || metadata
                    .layers
                    .iter()
                    .any(|node| node.clip_to.as_deref() == Some(&layer.id))
            {
                return Err("Release folder clipping before choosing Pass Through".into());
            }
        }
        if let Some(id) = &layer.clip_to {
            let base = metadata
                .layers
                .iter()
                .skip(i + 1)
                .filter(|node| node.parent == layer.parent)
                .find(|node| node.clip_to.is_none())
                .ok_or("Clipping needs a base layer below it")?;
            if base.id != *id || base.kind == "adjustment" {
                return Err(
                    "Keep clipped layers directly above their base, or release clipping first"
                        .into(),
                );
            }
            crate::psd::blend_ranges::validate_clipped_member(
                &layer.psd_metadata, &layer.kind, &base.kind, !base.effects.is_empty(),
                crate::psd::clipping::grouped(&base.psd_metadata),
            )?;
            crate::psd::blend_channels::validate_clipped_member(
                &layer.psd_metadata, &layer.kind, &base.kind, base.effects.iter().map(|effect| effect.kind.as_str()),
                crate::psd::clipping::grouped(&base.psd_metadata),
            )?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::scene::RasterCollector;
    use crate::engine::{Document, Engine, Layer};
    use serde_json::json;
    struct ZeroSources(Vec<Surface>);
    impl RasterCollector for ZeroSources {
        fn resident(&mut self, raster: &crate::raster::Raster) -> Result<RasterRef36> {
            assert!(raster.tiles.is_empty());
            let surface = Surface::zero(raster.width, raster.height, raster.depth)
                .map_err(|e| e.to_string())?;
            self.surface(&surface)
        }
        fn surface(&mut self, surface: &Surface) -> Result<RasterRef36> {
            self.0.push(surface.clone());
            Ok(RasterRef36 {
                id: self.0.len() as u32,
                width: surface.width(),
                height: surface.height(),
                depth: surface.depth(),
                retained: None,
            })
        }
    }
    fn fixture(depth: u16) -> Document {
        let mut doc = Document::new_depth(8, 8, depth).unwrap();
        doc.layers.clear();
        for (id, kind, parent) in [
            ("top", "paint", None),
            ("bottom", "paint", None),
            ("group", "group", None),
            ("child", "paint", Some("group")),
        ] {
            let mut layer = Layer::new(id, kind, 8, 8);
            layer.id = id.into();
            layer.parent = parent.map(String::from);
            layer.pixels = crate::raster::Raster::new_depth(8, 8, depth);
            doc.layers.push(layer);
        }
        doc
    }
    fn capture(doc: &Document, pool: &ResidentBudget) -> NativeDocument {
        let mut sources = ZeroSources(vec![]);
        let metadata = Document36::capture(doc, &mut sources).unwrap();
        let owner = MetadataCharge::reserve(pool, 128 * 1024, &|| Ok(())).unwrap();
        NativeDocument::from_pending(
            PendingScene::new(
                Admitted::from_parts(Arc::new(metadata), owner),
                RasterRoster::new(sources.0).unwrap(),
                pool.clone(),
                DiskBudget::new(1024 * 1024, 8).unwrap(),
            )
            .unwrap(),
        )
        .unwrap()
    }
    fn edit(base: &NativeDocument, command: &Value) -> Result<PreparedDelta> {
        let descriptor = CommandDescriptor::from_normalized(
            base,
            base.metadata().revision(),
            std::slice::from_ref(command),
            &|| Ok(()),
        )?;
        let token = descriptor.commands().next().unwrap().token();
        base.capture_descriptor(descriptor, "human", &[], 1, false, true)?
            .metadata_command(token, &|| Ok(()))
    }
    #[test]
    fn mask_step_enable_keeps_existing_cache_limits_including_disabled_masks() {
        for (depth, width, height, accepted) in [
            (8, 12000, 12000, false),
            (16, 12000, 12000, false),
            (16, 9000, 8000, true),
        ] {
            let pool = ResidentBudget::new(16 * 1024 * 1024).unwrap();
            let mut doc = fixture(depth);
            doc.width = width;
            doc.height = height;
            for layer in &mut doc.layers {
                layer.pixels = crate::raster::Raster::new_depth(width, height, depth);
            }
            doc.layers[0].mask = Some(crate::engine::Mask {
                enabled: false,
                linked: false,
                cache_key: "mask".into(),
                steps: vec![crate::engine::MaskStep {
                    id: "blur".into(),
                    kind: "blur".into(),
                    enabled: true,
                    weight: 1.,
                    value: 0.,
                    settings: Value::Null,
                    pixels: crate::raster::Raster::new_depth(width, height, depth),
                }],
            });
            let base = capture(&doc, &pool);
            doc.layers[0].mask.as_mut().unwrap().steps[0].value = 1.;
            assert_eq!(crate::mask::validate_budget(&doc.layers).is_ok(), accepted);
            let result = edit(
                &base,
                &serde_json::json!({"op":"mask.step.update","layer":"top","step":"blur","value":1}),
            );
            match result {
                Ok(prepared) => {
                    assert!(accepted);
                    assert!(!prepared
                        .preview()
                        .metadata()
                        .layer(0)
                        .unwrap()
                        .mask()
                        .unwrap()
                        .enabled());
                    assert_eq!(
                        prepared
                            .preview()
                            .metadata()
                            .layer(0)
                            .unwrap()
                            .mask()
                            .unwrap()
                            .steps()
                            .next()
                            .unwrap()
                            .value(),
                        1.
                    );
                }
                Err(error) => {
                    assert!(!accepted);
                    assert!(error.contains("128 MiB"), "{error}");
                }
            }
            assert_eq!(base.metadata().revision(), 0);
            assert_eq!(base.disk_budget().used().unwrap(), 0);
        }
    }
    #[test]
    fn rasterize_preserves_native_source_identity_and_undo_restores_the_definition() {
        assert_eq!(
            super::super::worker::handler("source.rasterize"),
            Some(super::super::worker::Handler::Metadata)
        );
        for depth in [8, 16] {
            let pool = ResidentBudget::new(16 * 1024 * 1024).unwrap();
            let mut resident = fixture(depth);
            resident.layers[0].source = Some(Arc::new(serde_json::from_value::<crate::source::Source>(json!({
                "width":8,"height":8,"content":{"kind":"text","text":"Saved text",
                    "font":{"family":"Unavailable fixture","postscript":"Unavailable-Regular",
                        "style":"normal","weight":400,"stretch":5,"index":0,"sha256":"0".repeat(64)},
                    "size":12.,"line_height":1.2,"align":"left","color":[12347,23459,34571,65535]}
            })).unwrap()));
            let mut current = capture(&resident, &pool);
            let original = current.clone();
            let source_key = current
                .source(current.layer_slot("top").unwrap())
                .unwrap()
                .key();
            let command = json!({"op":"source.rasterize","layer":"top"});
            let delta = edit(&current, &command).unwrap();
            assert!(current.metadata().layer(0).unwrap().source().is_some());
            assert!(delta
                .preview()
                .metadata()
                .layer(0)
                .unwrap()
                .source()
                .is_none());
            assert!(delta.outputs.is_empty());
            let mut history = NativeHistory::new(&pool, 4).unwrap();
            let commit = history
                .snapshot()
                .prepare_commit(
                    &current,
                    delta,
                    HistoryRequest {
                        actor: "human",
                        label: "Rasterize",
                        task: None,
                        gesture: None,
                    },
                    &|| Ok(()),
                )
                .unwrap();
            let gate = || Publication {
                actor: "human",
                leases: &[],
                now: 1,
                closed: false,
                write_available: true,
            };
            drop(history.commit(&mut current, gate(), commit).unwrap());
            assert!(edit(&current, &command).is_err());
            for (direction, editable) in [(Direction::Undo, true), (Direction::Redo, false)] {
                let moved = history
                    .snapshot()
                    .prepare_move(&current, direction, "human", &|| Ok(()))
                    .unwrap();
                drop(history.move_cursor(&mut current, gate(), moved).unwrap());
                assert_eq!(
                    current.metadata().layer(0).unwrap().source().is_some(),
                    editable
                );
                assert_eq!(
                    current
                        .source(current.layer_slot("top").unwrap())
                        .unwrap()
                        .key(),
                    source_key
                );
                assert_eq!(current.metadata().depth(), depth);
                if editable {
                    assert_eq!(
                        json!(current.metadata().layer(0).unwrap().source()),
                        json!(original.metadata().layer(0).unwrap().source())
                    );
                }
            }
            assert_eq!((history.undo_len(), history.redo_len()), (1, 0));
            assert_eq!(current.disk_budget().used().unwrap(), 0);
            // The same command still observes layer/ancestor locks and leases.
            resident.layers[0].locked = true;
            let locked = capture(&resident, &pool);
            assert!(edit(&locked, &command).is_err());
            resident.layers[0].locked = false;
            resident.layers[0].parent = Some("group".into());
            resident.layers[2].locked = true;
            let locked_parent = capture(&resident, &pool);
            assert!(edit(&locked_parent, &command).is_err());
            let descriptor = CommandDescriptor::from_normalized(
                &original,
                0,
                std::slice::from_ref(&command),
                &|| Ok(()),
            )
            .unwrap();
            let lease = crate::engine::Lease {
                id: "reserved".into(),
                owner: "other".into(),
                description: "Human source".into(),
                scopes: vec![crate::engine::Scope::layer("top")],
                expires: 100,
            };
            assert!(original
                .capture_descriptor(descriptor, "human", &[lease], 1, false, true)
                .is_err());
            drop((history, current, original, locked, locked_parent));
            assert_eq!(pool.used(), 0);
        }
    }
    #[test]
    fn pure_structural_operations_match_the_actual_legacy_engine_in_both_depths() {
        for depth in [8, 16] {
            for command in [
                json!({"op":"layer.update","layer":"top","name":"New name","opacity":0.45,"blend":"multiply","color":[1,2,3,4]}),
                json!({"op":"layer.update","layer":"top","fill_opacity":128}),
                json!({"op":"layer.update","layer":"top","color_label":"indigo"}),
                json!({"op":"layer.update","layer":"top","blend":"hard_mix","fill_opacity":128}),
                json!({"op":"layer.update","layer":"bottom","blend_clipped_as_group":false}),
                json!({"op":"layer.update","layer":"top","locked":true}),
                json!({"op":"layer.update","layer":"top","locked":false}),
                json!({"op":"layer.reorder","layer":"bottom","index":0}),
                json!({"op":"layer.clip","layer":"top","base":"bottom"}),
                json!({"op":"layer.parent","layer":"top","parent":"group"}),
                json!({"op":"layer.delete","layer":"group"}),
                json!({"op":"layer.delete","layer":"top"}),
            ] {
                let pool = ResidentBudget::new(16 * 1024 * 1024).unwrap();
                let mut original = fixture(depth);
                if command["locked"] == false {
                    original.layers[0].locked = true;
                    original.layers[0].psd_metadata.push(crate::psd::LayerMetadata {
                        key: "lspf".into(), data: 0x8000_0000u32.to_be_bytes().to_vec(),
                    });
                }
                let base = capture(&original, &pool);
                let source_key = base
                    .source(base.layer_slot("bottom").unwrap())
                    .unwrap()
                    .key();
                let mut legacy = Engine::new();
                legacy.resident_state_mut().expect("resident fixture").document = original;
                legacy
                    .edit(
                        "human",
                        std::slice::from_ref(&command),
                        None,
                        None,
                        "structural",
                    )
                    .unwrap();
                let mut sources = ZeroSources(vec![]);
                let expected = Document36::capture(&legacy.resident_state().expect("resident fixture").document, &mut sources).unwrap();
                let native = base
                    .publish("human", &[], 1, false, edit(&base, &command).unwrap())
                    .unwrap();
                assert_eq!(
                    serde_json::to_value(native.metadata()).unwrap(),
                    serde_json::to_value(expected).unwrap(),
                    "{depth} {command}"
                );
                assert_eq!(
                    native
                        .source(native.layer_slot("bottom").unwrap())
                        .unwrap()
                        .key(),
                    source_key
                );
                drop(native);
                drop(base);
                assert_eq!(pool.used(), 0);
            }
        }
    }
    #[test]
    fn creation_and_later_update_share_the_same_transaction_without_source_io() {
        let pool = ResidentBudget::new(16 * 1024 * 1024).unwrap();
        let base = capture(&fixture(16), &pool);
        let commands = [
            json!({"op":"layer.add","kind":"fill","name":"New layer","color":[1,2,3,4],"parent":"group"}),
            json!({"op":"layer.update","layer":"top","opacity":0.5}),
        ];
        let descriptor =
            CommandDescriptor::from_normalized(&base, 0, &commands, &|| Ok(())).unwrap();
        let mut transaction = base
            .capture_descriptor(descriptor, "human", &[], 1, false, true)
            .unwrap()
            .transaction()
            .unwrap();
        for _ in 0..2 {
            let step = transaction.next(&|| Ok(())).unwrap();
            let token = step
                .descriptor()
                .unwrap()
                .commands()
                .next()
                .unwrap()
                .token();
            let prepared = step.metadata_command(token, &|| Ok(())).unwrap();
            transaction.complete(prepared, &|| Ok(())).unwrap();
        }
        let native = base
            .publish(
                "human",
                &[],
                1,
                false,
                transaction.finish(&|| Ok(())).unwrap(),
            )
            .unwrap();
        assert_eq!(native.metadata().revision(), 1);
        assert_eq!(native.metadata().layers().len(), 5);
        let new = native.metadata().layer(0).unwrap();
        assert_eq!(new.name(), "New layer");
        assert_eq!(new.kind(), "fill");
        assert_eq!(new.color(), [1, 2, 3, 4]);
        assert_eq!(new.parent(), Some("group"));
        assert_eq!(new.pixels().depth(), 16);
        assert!(native.source(new.pixels().slot()).unwrap().is_known_zero());
        assert_eq!(native.disk_budget().used().unwrap(), 0);
        drop(native);
        drop(base);
        assert_eq!(pool.used(), 0);
    }
    #[test]
    fn invalid_clipping_parent_cycles_locks_and_foreign_tokens_preserve_the_base() {
        let pool = ResidentBudget::new(16 * 1024 * 1024).unwrap();
        let mut resident = fixture(16);
        resident.layers[0].locked = true;
        let base = capture(&resident, &pool);
        let before = base.key();
        for command in [
            json!({"op":"layer.update","layer":"top","name":"locked"}),
            json!({"op":"layer.parent","layer":"group","parent":"group"}),
            json!({"op":"layer.clip","layer":"bottom","base":"top"}),
        ] {
            assert!(edit(&base, &command).is_err());
            assert_eq!(base.key(), before);
        }
        assert!(edit(
            &base,
            &json!({"op":"layer.update","layer":"top","visible":false})
        )
        .is_ok());
        let first = CommandDescriptor::from_normalized(
            &base,
            0,
            &[json!({"op":"layer.update","layer":"top","visible":false})],
            &|| Ok(()),
        )
        .unwrap();
        let token = first.commands().next().unwrap().token();
        let second = CommandDescriptor::from_normalized(
            &base,
            0,
            &[json!({"op":"layer.update","layer":"top","visible":false})],
            &|| Ok(()),
        )
        .unwrap();
        assert!(base
            .capture_descriptor(second, "human", &[], 1, false, true)
            .unwrap()
            .metadata_command(token, &|| Ok(()))
            .is_err());
        drop(first);
        drop(base);
        assert_eq!(pool.used(), 0);
    }
}
