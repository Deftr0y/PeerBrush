//! Duplicate headers and identities while sharing every native source owner.
use super::*;
use crate::{engine::scene::MetadataPayload, layer_clipboard::Forest};

pub(super) fn requested_root_ids<'a>(
    base: &NativeDocument,
    command: &'a serde_json::Value,
) -> Result<Option<&'a serde_json::Map<String, serde_json::Value>>> {
    if command["layers"].as_array().is_some_and(|layers| layers.len() > 100) {
        return Err("Choose 1–100 layers to copy".into());
    }
    let roots = crate::grouping::requested(command)?;
    let forest = Forest::new(base.into(), &roots)?;
    crate::layer_clipboard::checked_duplicate_ids(
        base.into(),
        forest.roots().map(|i| base.core.metadata.layers[i].id.as_str()),
        command.get("root_ids"),
    )
}

pub(super) fn prepare(edit: CapturedEdit, check: &dyn Fn() -> Result<()>) -> Result<PreparedDelta> {
    let checked = || {
        edit.cancellation.check()?;
        check()
    };
    checked()?;
    let descriptor = edit.descriptor().ok_or("Duplicate capture missing")?;
    let command = descriptor
        .commands()
        .next()
        .ok_or("Duplicate command missing")?;
    let selected = command.parameters()["op"] == "layer.duplicate_selection";
    if descriptor.commands().len() != 1
        || !(command.method() == Method::Metadata
            && command.parameters()["op"] == "layer.duplicate"
            || selected
                && command.method() == Method::Import
                && edit.document().metadata().selection().is_none())
    {
        return Err("Duplicate producer differs from its normalized command".into());
    }
    let c = command.parameters();
    let base = edit.document();
    if base.metadata().read_only() {
        return Err("This PSD is read-only".into());
    }
    if !selected && c["layers"].as_array().is_some_and(|v| v.len() > 100) {
        return Err("Choose 1–100 layers to copy".into());
    }
    let bytes = if selected {
        c["layer"].as_str().map_or(0, str::len) + 8192
    } else {
        c["layers"].as_array().into_iter().flatten().try_fold(
            c["layer"].as_str().map_or(0, str::len) + 8192,
            |n, value| {
                n.checked_add(value.as_str().map_or(0, str::len) + std::mem::size_of::<String>())
                    .ok_or("Duplicate request footprint overflow")
            },
        )?
    };
    let _request = MetadataCharge::reserve(base.budget(), bytes, &checked)?;
    let ids = if selected {
        vec![c["layer"]
            .as_str()
            .ok_or("Choose an active layer to duplicate")?
            .into()]
    } else {
        crate::grouping::requested(c)?
    };
    let forest = Forest::new(base.into(), &ids)?;
    let root_ids = crate::layer_clipboard::checked_duplicate_ids(
        base.into(),
        forest.roots().map(|i| base.core.metadata.layers[i].id.as_str()),
        (!selected).then(|| c.get("root_ids")).flatten(),
    )?;
    let n = base.core.metadata.layers.len();
    let count = forest.nodes().count();
    if n + count > 100 {
        return Err("Initial version supports up to 100 layers".into());
    }
    for root in forest.roots() {
        super::patch::unlocked(&base.core.metadata, &base.core.metadata.layers[root].id)?;
    }
    let mut extra = [0; 100];
    let mut roots = [0; 100];
    let mut root = 0;
    let mut bytes = 4096usize;
    for (j, i) in forest.nodes().enumerate() {
        extra[j] = i;
        if forest.is_root(i) {
            root = i;
        }
        roots[j] = root;
        let layer = &base.core.metadata.layers[i];
        // All fresh identifiers, parent/clip remaps and the replacement root name
        // coexist with their original measured headers until binding finishes.
        bytes = bytes
            .checked_add(
                64 * (4 + layer.effects.len() + layer.mask.as_ref().map_or(0, |m| m.steps.len())),
            )
            .and_then(|v| {
                v.checked_add(if forest.is_root(i) {
                    layer.name.len() + 5
                } else {
                    0
                })
            })
            .ok_or("Duplicate header growth overflow")?;
    }
    let growth = MetadataCharge::reserve(base.budget(), bytes, &checked)?;
    let mut mapping: [Option<String>; 100] = std::array::from_fn(|_| None);
    for &i in &extra[..count] {
        let copied = root_ids
            .and_then(|ids| ids.get(&base.core.metadata.layers[i].id))
            .and_then(serde_json::Value::as_str);
        mapping[i] = Some(copied.map_or_else(crate::engine::id, str::to_owned));
    }
    let copied = base
        .core
        .metadata
        .copy_runtime_with_layers(&extra[..count], base.budget(), &checked)?
        .retain_owner(&growth, &checked)?;
    let intent = edit.planned_intent(Intent::from_scopes(
        IntentKind::Capture,
        command.write_scopes(),
        base.budget(),
    )?)?;
    let core = copied.rehome_unique(|mut metadata, _| {
        for (j, &i) in extra[..count].iter().enumerate() {
            checked()?;
            let old = &base.core.metadata.layers[i];
            let layer = &mut metadata.layers[n + j];
            layer.id = mapping[i].as_ref().unwrap().clone();
            if forest.is_root(i) {
                let length = old.name.len() + 5;
                let mut name = String::new();
                name.try_reserve_exact(length)
                    .map_err(|_| "Duplicate name allocation refused")?;
                if name.capacity() > length {
                    return Err("Duplicate name exceeds admission".into());
                }
                name.push_str(&old.name);
                name.push_str(" copy");
                layer.name = name;
            } else {
                let parent = base
                    .core
                    .metadata
                    .layers
                    .iter()
                    .position(|l| Some(l.id.as_str()) == old.parent.as_deref())
                    .ok_or("Duplicate parent missing")?;
                layer.parent = Some(
                    mapping[parent]
                        .as_ref()
                        .ok_or("Duplicate parent outside copied forest")?
                        .clone(),
                );
            }
            if let Some(id) = &old.clip_to {
                let target = base
                    .core
                    .metadata
                    .layers
                    .iter()
                    .position(|l| l.id == *id)
                    .ok_or("Duplicate clipping base missing")?;
                layer.clip_to = Some(
                    mapping[target]
                        .as_ref()
                        .ok_or("Duplicate clipping base outside copied forest")?
                        .clone(),
                );
            }
            fresh_details(layer, base.budget(), &checked)?;
        }
        let block = forest
            .nodes()
            .any(|i| base.core.metadata.layers[i].clip_to.is_some());
        let first = forest.roots().min().ok_or("Duplicate roots missing")?;
        let mut order = [0; 100];
        let mut out = 0;
        for i in 0..n {
            if (block && i == first) || (!block && forest.is_root(i)) {
                for (j, &root) in roots[..count].iter().enumerate() {
                    if block || root == i {
                        order[out] = n + j;
                        out += 1;
                    }
                }
            }
            order[out] = i;
            out += 1;
        }
        if out != n + count {
            return Err("Duplicate order does not cover its layer roster".into());
        }
        let mut current: [usize; 100] = std::array::from_fn(|i| i);
        for (i, &wanted) in order[..out].iter().enumerate() {
            let at = (i..out)
                .find(|&j| current[j] == wanted)
                .ok_or("Duplicate order is not a permutation")?;
            metadata.layers.swap(i, at);
            current.swap(i, at);
        }
        super::metadata::validate_clipping(&metadata)?;
        // Copied references still name the captured sources, and are not yet a
        // canonical sequential roster. Rebuild renumbers before full validation.
        let mut locations = 0usize;
        metadata.remap_rasters(|_| {
            locations = locations
                .checked_add(1)
                .ok_or("Duplicate raster count overflow")?;
            Ok(())
        })?;
        let roster = rebuild_from(
            &mut metadata,
            locations,
            base.budget(),
            base.disk_budget(),
            &checked,
            |reference| base.roster().get(reference).cloned(),
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
    Ok(PreparedDelta {
        base: base.key(),
        candidate: NativeDocument { core },
        cancellation: edit.cancellation,
        intent,
        outputs: vec![],
        admission: edit.admission,
    })
}

pub(super) fn fresh_details(
    layer: &mut Layer36,
    pool: &ResidentBudget,
    check: &dyn Fn() -> Result<()>,
) -> Result<()> {
    for effect in &mut layer.effects {
        effect.id = crate::engine::id();
    }
    if let Some(mask) = &mut layer.mask {
        for step in &mut mask.steps {
            step.id = crate::engine::id();
        }
    }
    if layer
        .psd_metadata
        .iter()
        .any(|entry| !crate::psd::metadata_copies(entry))
    {
        let payload: Admitted<Vec<crate::psd::LayerMetadata>> =
            copy_admitted(&layer.psd_metadata, pool, true, check)?;
        drop(payload.map_unique(|mut entries, _| {
            entries.retain(crate::psd::metadata_copies);
            layer.psd_metadata = MetadataPayload::from(entries).bind(pool, check)?;
            Ok(())
        })?);
    }
    Ok(())
}
