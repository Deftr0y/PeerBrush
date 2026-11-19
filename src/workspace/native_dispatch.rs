//! Capture the configured workspace service without retaining an engine or
//! registry lock. Actual source admission and I/O happen only after capture.
use super::source_runtime::SourceReadSession;
use super::{
    source_runtime::{SourceRuntime, SourceSession},
    *,
};
use crate::engine::scene::SceneSnapshot;
use std::sync::{MutexGuard, TryLockError};
use std::time::{Duration, Instant};

const PROJECT_BUSY: &str = "Project is busy; source capture deferred";
const WORKSPACE_BUSY: &str = "Workspace is busy; source capture deferred";

pub(crate) fn retryable_source_read(error: &str) -> bool {
    matches!(error, PROJECT_BUSY | WORKSPACE_BUSY | source_runtime::INVENTORY_CHECK_BUSY)
        || error == crate::large_document::Error::Busy.to_string()
}

/// For off-UI source admission and read-only checks. Retrying publication is unsafe.
pub(crate) fn retry_source_read<T>(mut read: impl FnMut() -> Result<T, String>) -> Result<T, String> {
    let deadline = Instant::now() + Duration::from_secs(1);
    loop {
        match read() {
            Err(error) if retryable_source_read(&error) && Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(10));
            }
            result => return result,
        }
    }
}

pub(crate) fn try_source_engine(shared: &Shared) -> Result<MutexGuard<'_, Engine>, String> {
    shared.try_lock().map_err(|error| match error {
        TryLockError::WouldBlock => PROJECT_BUSY.into(),
        TryLockError::Poisoned(_) => "Project source state is unavailable".into(),
    })
}

/// Never attach a new registry or invent a fallback source root. Native work
/// must use the exact service configured by the authenticated app connection.
pub(crate) fn runtime_for(shared: &Shared) -> Result<Arc<SourceRuntime>, String> {
    configured_runtime_for(shared)?.ok_or_else(|| "Native source service is not configured".into())
}

pub(crate) fn configured_runtime_for(
    shared: &Shared,
) -> Result<Option<Arc<SourceRuntime>>, String> {
    let (registry, project, native) = {
        let engine = try_source_engine(shared)?;
        engine.ensure_open()?;
        (
            engine.workspace.as_ref().and_then(Weak::upgrade),
            engine.project_id.clone(),
            engine.native_document().is_some(),
        )
    };
    let Some(registry) = registry else {
        return if native {
            Err("Project has no configured source workspace".into())
        } else {
            Ok(None)
        };
    };
    let state = registry.try_lock().map_err(|error| match error {
        TryLockError::WouldBlock => WORKSPACE_BUSY,
        TryLockError::Poisoned(_) => "Workspace source state is unavailable",
    })?;
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
    if native && state.source_runtime.is_none() {
        return Err("Native source service is not configured".into());
    }
    Ok(state.source_runtime.clone())
}

/// Called off-lock with the exact immutable scene captured for this project.
/// The ordinary SourceSession retains profile/instance/inventory admission;
/// neither metadata IDs nor this accessor grant filesystem publication rights.
pub(crate) fn begin_scene(shared: &Shared, scene: &SceneSnapshot) -> Result<SourceSession, String> {
    begin_scene_with(shared, scene, SourceRuntime::begin)
}
pub(crate) fn begin_edit_scene(
    shared: &Shared,
    scene: &SceneSnapshot,
) -> Result<SourceSession, String> {
    begin_scene_with(shared, scene, SourceRuntime::begin_foreground)
}
fn begin_scene_with(
    shared: &Shared,
    scene: &SceneSnapshot,
    begin: impl FnOnce(&Arc<SourceRuntime>) -> Result<SourceSession, String>,
) -> Result<SourceSession, String> {
    let runtime = runtime_for(shared)?;
    if !scene.budget().same_pool(&runtime.resident())
        || !scene.disk_budget().same_pool(&runtime.disk())
    {
        return Err("Captured scene belongs to another source service".into());
    }
    validate_scene(shared, scene)?;
    let session = begin(&runtime)?;
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
    let engine = try_source_engine(shared)?;
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

    #[test]
    fn import_source_admission_retries_for_the_original_tab_after_switching() {
        let fixture = crate::engine::native::worker::tests::Fixture::new();
        let (runtime, _connection) = fixture.runtime();
        let first = shared();
        let registry = attach(&first);
        let second = new_project(&first, 8, 8, 16, None).unwrap();
        let second_id = second.lock().unwrap().project_id.clone();
        select(&first, &second_id).unwrap();

        let mut held = registry.lock().unwrap();
        held.source_runtime = Some(runtime.clone());
        let (blocked, waiting) = std::sync::mpsc::channel();
        let observed = std::thread::scope(|scope| {
            let worker = scope.spawn(|| {
                let mut blocked = Some(blocked);
                retry_source_read(|| {
                    let result = runtime_for(&first);
                    if matches!(&result, Err(error) if error == WORKSPACE_BUSY) {
                        if let Some(blocked) = blocked.take() {
                            blocked.send(()).unwrap();
                        }
                    }
                    result
                })
            });
            let received = waiting.recv_timeout(Duration::from_secs(5));
            drop(held);
            received.unwrap();
            worker.join().unwrap().unwrap()
        });
        assert!(Arc::ptr_eq(&observed, &runtime));
    }
}
