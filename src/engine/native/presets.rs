//! Freeze the existing library semantics before native scope capture. Borrowed
//! preset plans admit simultaneous command/settings copies before resolution.
use super::*;
use crate::server::Shared;
use serde_json::Value;

fn charge<T: Serialize>(
    commands: &[Value],
    presets: &[Option<&T>],
    pool: &ResidentBudget,
    check: &dyn Fn() -> Result<()>,
) -> Result<Arc<MetadataCharge>> {
    let copies = metadata_working_charge(
        &(commands, commands, presets, presets, presets),
        pool,
        check,
    )?;
    // Existing normalizers add bounded scalar defaults and command headers;
    // variable-sized input/settings are measured in the borrowed plan above.
    let defaults = MetadataCharge::reserve(pool, commands.len() * 16 * 1024 + 4096, check)?;
    MetadataCharge::combine(&[copies, defaults], check)
}
pub(crate) fn freeze_shared(
    shared: &Shared,
    document: &NativeDocument,
    commands: &[Value],
    check: &dyn Fn() -> Result<()>,
) -> Result<Option<Admitted<Vec<Value>>>> {
    if !commands
        .iter()
        .any(|command| command.get("preset").is_some())
    {
        return Ok(None);
    }
    if commands.is_empty() || commands.len() > 100 {
        return Err("A batch must contain 1-100 commands".into());
    }
    let (filters, brushes) = {
        let engine = shared.lock().unwrap();
        engine.ensure_open()?;
        if engine
            .native_document()
            .is_none_or(|current| current.key() != document.key())
        {
            return Err("Preset source changed before native capture".into());
        }
        (engine.filter_library.clone(), engine.brush_library.clone())
    };
    let mut frozen = None;
    if commands.iter().any(|c| {
        c["op"].as_str().is_some_and(|op| op.starts_with("filter.")) && c.get("preset").is_some()
    }) {
        let library = filters.lock().unwrap();
        let mut selected = [None; 100];
        for (index, command) in commands.iter().enumerate() {
            if command["op"]
                .as_str()
                .is_some_and(|op| op.starts_with("filter."))
            {
                if let Some(preset) = command.get("preset") {
                    selected[index] = Some(
                        library.get_ref(preset.as_str().ok_or("Filter preset must be an ID")?)?,
                    );
                }
            }
        }
        let owner = charge(
            commands,
            &selected[..commands.len()],
            document.budget(),
            check,
        )?;
        let resolved = library.resolve_commands(commands)?;
        check()?;
        frozen = Some(Admitted::from_parts(Arc::new(resolved), owner));
    }
    let current = frozen.as_deref().map_or(commands, Vec::as_slice);
    if current
        .iter()
        .any(|command| command.get("preset").is_some())
    {
        let library = brushes.lock().unwrap();
        let mut selected = [None; 100];
        for (index, command) in current.iter().enumerate() {
            if let Some(preset) = command.get("preset") {
                selected[index] =
                    Some(library.get_ref(preset.as_str().ok_or("Brush preset must be an ID")?)?);
            }
        }
        let owner = charge(
            current,
            &selected[..current.len()],
            document.budget(),
            check,
        )?;
        let resolved = library.resolve_commands(current)?;
        check()?;
        frozen = Some(Admitted::from_parts(Arc::new(resolved), owner));
    }
    Ok(frozen)
}
