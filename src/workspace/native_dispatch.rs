//! Capture the configured workspace service without retaining an engine or
//! registry lock. Actual source admission and I/O happen only after capture.
use super::source_runtime::SourceReadSession;
use super::{
    source_runtime::{SourceRuntime, SourceSession},
    *,
};
use crate::engine::scene::SceneSnapshot;

/// Never attach a new registry or invent a fallback source root. Native work
/// must use the exact service configured by the authenticated app connection.
pub(crate) fn runtime_for(shared: &Shared) -> Result<Arc<SourceRuntime>, String> {
    let (registry, project) = {
        let engine = shared
            .try_lock()
            .map_err(|_| "Project is busy; source capture deferred")?;
        engine.ensure_open()?;
        let registry = engine
            .workspace
            .as_ref()
            .and_then(Weak::upgrade)
            .ok_or("Project has no configured source workspace")?;
        (registry, engine.project_id.clone())
    };
    let state = registry
        .try_lock()
        .map_err(|_| "Workspace is busy; source capture deferred")?;
    if state.exiting {
        return Err("Workspace is exiting; source capture refused".into());
    }
    if !state.entries.iter().any(|entry| {
        entry.id == project
            && entry
                .handle
                .get()
                .is_some_and(|registered| Arc::ptr_eq(&registered, shared))
    }) {
        return Err("Project is no longer registered in its source workspace".into());
    }
    state
        .source_runtime
        .clone()
        .ok_or_else(|| "Native source service is not configured".into())
}

/// Called off-lock with the exact immutable scene captured for this project.
/// The ordinary SourceSession retains profile/instance/inventory admission;
/// neither metadata IDs nor this accessor grant filesystem publication rights.
pub(crate) fn begin_scene(shared: &Shared, scene: &SceneSnapshot) -> Result<SourceSession, String> {
    let runtime = runtime_for(shared)?;
    if !scene.budget().same_pool(&runtime.resident())
        || !scene.disk_budget().same_pool(&runtime.disk())
    {
        return Err("Captured scene belongs to another source service".into());
    }
    validate_scene(shared, scene)?;
    let session = runtime.begin()?;
    // Source admission may take time. Do not return a newly acquired worker
    // session for a project replaced or closed while that admission was running.
    validate_scene(shared, scene)?;
    session.check()?;
    Ok(session)
}
/// Display-only capture preserves blank/health profile absence. It has no source
/// mutation API; nonzero physical sources retain ordinary inventory admission.
pub(crate) fn begin_read_scene(
    shared: &Shared,
    scene: &SceneSnapshot,
) -> Result<SourceReadSession, String> {
    let runtime = runtime_for(shared)?;
    validate_scene(shared, scene)?;
    let session = runtime.begin_read(scene)?;
    validate_scene(shared, scene)?;
    session.check_scene(scene)?;
    Ok(session)
}
fn validate_scene(shared: &Shared, scene: &SceneSnapshot) -> Result<(), String> {
    let engine = shared
        .try_lock()
        .map_err(|_| "Project changed or is busy during source admission")?;
    engine.ensure_open()?;
    let view = crate::engine::view::current(&engine);
    if engine.project_id != scene.identity().project_id
        || view.id() != scene.identity().document_id
        || view.revision() != scene.identity().revision
        || !matches!(view, crate::engine::view::DocumentView::Native(document) if document.key() == scene.key())
    {
        return Err("Captured project changed before source work began".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn shared() -> Shared {
        Arc::new(Mutex::new(Engine::new()))
    }
    #[test]
    fn missing_service_never_creates_a_fallback_registry() {
        let engine = shared();
        assert!(runtime_for(&engine).is_err());
        assert!(engine.lock().unwrap().workspace.is_none());
        let registry = attach(&engine);
        assert!(runtime_for(&engine).is_err());
        assert!(registry.lock().unwrap().source_runtime.is_none());
    }
    #[test]
    fn contention_closed_exit_and_replaced_registration_refuse_without_io() {
        let engine = shared();
        let registry = attach(&engine);
        {
            let _held = engine.lock().unwrap();
            assert!(runtime_for(&engine).is_err());
        }
        {
            let _held = registry.lock().unwrap();
            assert!(runtime_for(&engine).is_err());
        }
        engine.lock().unwrap().closed = true;
        assert!(runtime_for(&engine).is_err());
        engine.lock().unwrap().closed = false;
        registry.lock().unwrap().exiting = true;
        assert!(runtime_for(&engine).is_err());
        registry.lock().unwrap().exiting = false;
        registry.lock().unwrap().entries[0].handle = Handle::Owned(shared());
        assert!(runtime_for(&engine).is_err());
    }
}
