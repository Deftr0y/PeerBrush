//! Document filter commands share resident semantics and native source owners.
use super::*;
use crate::engine::scene::{Effect36, MetadataPayload};

pub(super) fn prepare(edit: CapturedEdit, check: &dyn Fn() -> Result<()>) -> Result<PreparedDelta> {
    let checked = || {
        edit.cancellation.check()?;
        check()
    };
    checked()?;
    let descriptor = edit.descriptor().ok_or("Filter capture missing")?;
    let command = descriptor
        .commands()
        .next()
        .ok_or("Filter command missing")?;
    if descriptor.commands().len() != 1
        || command.method() != Method::Effects
        || !matches!(
            command.parameters()["op"].as_str(),
            Some("filter.add" | "filter.update" | "filter.delete" | "filter.reorder")
        )
    {
        return Err("Filter producer differs from its normalized command".into());
    }
    let base = edit.document();
    let original = &base.core.metadata;
    if original.read_only {
        return Err("This document is read-only".into());
    }
    if original.layers.iter().any(|layer| layer.locked) {
        return Err("Unlock layers and folders before changing whole-image filters".into());
    }
    if original.filters.len() > crate::filters::LIMIT
        || original
            .filters
            .iter()
            .any(|filter| !filter.clips.is_empty())
    {
        return Err("Unsupported native filter stack".into());
    }
    // Bounded metadata copies only. Charge the temporary stack, normalization
    // and validation before calling the shared resident command implementation.
    let mut bytes = 64 * 1024;
    crate::effects::regional::metadata_bytes(command.parameters(), 0, &mut bytes)?;
    for filter in &original.filters {
        checked()?;
        bytes = bytes
            .checked_add(filter.id.len() + filter.kind.len())
            .ok_or("Filter metadata footprint overflow")?;
        crate::effects::regional::metadata_bytes(&filter.settings, 0, &mut bytes)?;
    }
    let bytes = bytes
        .checked_mul(4)
        .ok_or("Filter scratch footprint overflow")?;
    let _scratch = MetadataCharge::reserve(base.budget(), bytes, &checked)?;
    let mut filters = Vec::new();
    filters
        .try_reserve_exact(crate::filters::LIMIT)
        .map_err(|_| "Filter metadata allocation refused")?;
    for filter in &original.filters {
        checked()?;
        filters.push(crate::effects::Effect {
            id: filter.id.clone(),
            kind: filter.kind.clone(),
            enabled: filter.enabled,
            weight: filter.weight,
            settings: Arc::new((*filter.settings).clone()),
        });
    }
    crate::filters::apply_stack(&mut filters, command.parameters())?;
    crate::filters::validate_regional(&filters)?;
    checked()?;
    let header_bytes = filters.iter().try_fold(
        4096 + crate::filters::LIMIT * std::mem::size_of::<Effect36>(),
        |n, filter| {
            n.checked_add(filter.id.len() + filter.kind.len())
                .ok_or("Filter header footprint overflow")
        },
    )?;
    let growth = MetadataCharge::reserve(base.budget(), header_bytes, &checked)?;
    let copied =
        copy_metadata(original, base.budget(), &checked)?.retain_owner(&growth, &checked)?;
    let intent = edit.planned_intent(Intent::from_scopes(
        IntentKind::Capture,
        command.write_scopes(),
        base.budget(),
    )?)?;
    let core = copied.rehome_unique(|mut metadata, _| {
        let mut replacement = Vec::new();
        replacement
            .try_reserve_exact(filters.len())
            .map_err(|_| "Filter header allocation refused")?;
        for filter in filters {
            checked()?;
            let old = original.filters.iter().find(|old| old.id == filter.id);
            let settings = if let Some(old) = old.filter(|old| *old.settings == *filter.settings) {
                old.settings.shared()?
            } else {
                MetadataPayload::from(
                    Arc::try_unwrap(filter.settings)
                        .map_err(|_| "Filter settings ownership changed")?,
                )
                .bind(base.budget(), &checked)?
            };
            replacement.push(Effect36 {
                id: filter.id,
                kind: filter.kind,
                enabled: filter.enabled,
                weight: filter.weight,
                settings,
                clips: Vec::new(),
            });
        }
        metadata.filters = replacement;
        metadata.revision = metadata
            .revision
            .checked_add(1)
            .ok_or("Native revision overflow")?;
        checked()?;
        Core::prepare(
            metadata,
            base.core.roster.clone(),
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
