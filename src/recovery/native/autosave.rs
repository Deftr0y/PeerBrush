//! Native recovery publication and bounded marker retention for the worker.
use super::*;
use crate::{
    engine::{native::DocumentKey, scene::SceneSnapshot},
    workspace::source_runtime::{CheckpointScope, SourceRuntime},
};
use std::sync::atomic::Ordering;

pub(in crate::recovery) const FOREGROUND_ACTIVE: &str =
    "Native checkpoint deferred while foreground workspace work is active";

fn begin_checkpoint(runtime: &SourceRuntime) -> Result<CheckpointScope<'_>, String> {
    runtime
        .try_checkpoint()?
        .ok_or_else(|| FOREGROUND_ACTIVE.into())
}

fn foreground_idle(checkpoint: &CheckpointScope<'_>) -> Result<(), String> {
    if checkpoint.interrupted() {
        return Err(FOREGROUND_ACTIVE.into());
    }
    Ok(())
}

#[derive(Clone)]
pub(in crate::recovery) struct Saved {
    pub(in crate::recovery) key: DocumentKey,
    pub(in crate::recovery) prune_pending: bool,
    snapshot: [u8; 16],
    runtime: Arc<SourceRuntime>,
    _admission: Arc<MetadataCharge>,
}
pub(in crate::recovery) struct Outcome {
    pub(in crate::recovery) saved: Saved,
    pub(in crate::recovery) error: Option<String>,
}
impl Saved {
    pub(in crate::recovery) fn check_runtime(
        &self,
        workspace: &workspace::Registry,
    ) -> Result<(), String> {
        check_runtime(workspace, &self.runtime)
    }
}

pub(in crate::recovery) fn checkpoint(
    workspace: &workspace::Registry,
    dir: &Path,
    shared: &server::Shared,
    scene: &SceneSnapshot,
    previous: Option<&Saved>,
    check: &dyn Fn() -> Result<(), String>,
) -> Result<Outcome, String> {
    if !DISCARD_AVAILABLE {
        return Err("Native autosave retention is unavailable on this platform; existing recoveries retained".into());
    }
    let runtime = workspace::native_dispatch::runtime_for(shared)?;
    if previous.is_some_and(|saved| !Arc::ptr_eq(&saved.runtime, &runtime)) {
        return Err("Native recovery source service changed; existing recoveries retained".into());
    }
    let checkpoint = begin_checkpoint(&runtime)?;
    let validate = || {
        check()?;
        foreground_idle(&checkpoint)?;
        validate_source(workspace, shared, scene, &runtime)
    };
    validate()?;
    let project = &scene.identity().project_id;
    let catalog = complete_catalog(workspace, &validate)?;
    // Failed retention never permits another version to accumulate unboundedly.
    let protected = previous
        .filter(|saved| {
            catalog
                .entries()
                .iter()
                .any(|entry| native_id(entry) == Some(saved.snapshot))
        })
        .map(|saved| saved.snapshot);
    prune(
        workspace,
        &runtime,
        &catalog,
        project,
        VERSIONS_PER_PROJECT,
        protected,
        &validate,
    )?;
    if let Some(saved) = previous.filter(|saved| saved.key == scene.key()) {
        let mut saved = saved.clone();
        saved.prune_pending = false;
        return Ok(Outcome { saved, error: None });
    }
    // This charge also covers the bounded project key and worker map entry.
    let admission = MetadataCharge::reserve(scene.budget(), 4096, &validate)?;
    let snapshot = *uuid::Uuid::new_v4().as_bytes();
    let cleanup_check = || {
        check()?;
        check_runtime(workspace, &runtime)
    };
    {
        let session = workspace::native_dispatch::begin_scene(shared, scene)?;
        let capture = backed::Capture::from_native(scene, &session, limits(), &validate)?;
        admit(&catalog, dir, project, capture.encoded_len()?, true)?;
        capture.publish_with_completion(snapshot, &session, &validate, &cleanup_check)?;
    }
    // No source session survives into exclusive marker disposal. The captured
    // scene and registered checkpoints keep the actual pixel owners alive.
    let retention_check = || {
        foreground_idle(&checkpoint)?;
        cleanup_check()
    };
    let cleanup = complete_catalog(workspace, &retention_check).and_then(|catalog| {
        prune(
            workspace,
            &runtime,
            &catalog,
            project,
            VERSIONS_PER_PROJECT,
            Some(snapshot),
            &retention_check,
        )
    });
    let prune_pending = cleanup.is_err();
    let error = cleanup
        .err()
        .filter(|error| error != FOREGROUND_ACTIVE)
        .map(|error| format!("Native checkpoint is durable; retention pending: {error}"));
    drop(checkpoint);
    Ok(Outcome {
        saved: Saved {
            key: scene.key(),
            prune_pending,
            snapshot,
            runtime,
            _admission: admission,
        },
        error,
    })
}

fn validate_source(
    workspace: &workspace::Registry,
    shared: &server::Shared,
    scene: &SceneSnapshot,
    runtime: &Arc<SourceRuntime>,
) -> Result<(), String> {
    check_runtime(workspace, runtime)?;
    let engine = shared
        .try_lock()
        .map_err(|_| "Project is busy; native checkpoint deferred")?;
    engine.ensure_open()?;
    if engine.file_operations.load(Ordering::Acquire) != 0 {
        return Err(FOREGROUND_ACTIVE.into());
    }
    let document = engine
        .native_document()
        .ok_or("Native recovery authority changed; checkpoint deferred")?;
    if engine.project_id != scene.identity().project_id
        || document.key() != scene.key()
        || document.metadata().read_only()
        || engine.saved_revision == document.metadata().revision()
    {
        return Err("Native recovery source changed or was saved; checkpoint deferred".into());
    }
    Ok(())
}
fn check_runtime(
    workspace: &workspace::Registry,
    runtime: &Arc<SourceRuntime>,
) -> Result<(), String> {
    let state = workspace
        .try_lock()
        .map_err(|_| "Workspace is busy; native checkpoint deferred")?;
    if state.exiting
        || !state
            .source_runtime
            .as_ref()
            .is_some_and(|current| Arc::ptr_eq(current, runtime))
    {
        return Err("Native recovery source service changed; existing recoveries retained".into());
    }
    Ok(())
}

fn complete_catalog(
    workspace: &workspace::Registry,
    check: &dyn Fn() -> Result<(), String>,
) -> Result<Catalog, String> {
    let catalog = super::catalog(workspace, check)?;
    if catalog.error().is_some()
        || catalog.entries().iter().any(|entry| {
            entry.error.is_some()
                || entry.project_id.is_none()
                || entry.bytes.is_none()
                || entry.timestamp_ms.is_none()
        })
    {
        return Err(
            "Native recovery inventory is incomplete; existing checkpoints retained".into(),
        );
    }
    Ok(catalog)
}
fn native_id(entry: &NativeSnapshot) -> Option<[u8; 16]> {
    entry
        .snapshot
        .strip_prefix("native:")
        .and_then(|id| uuid::Uuid::parse_str(id).ok())
        .map(|id| *id.as_bytes())
}
fn prune(
    workspace: &workspace::Registry,
    runtime: &Arc<SourceRuntime>,
    catalog: &Catalog,
    project: &str,
    keep: usize,
    protected: Option<[u8; 16]>,
    check: &dyn Fn() -> Result<(), String>,
) -> Result<(), String> {
    let protected = protected
        .map(|id| {
            catalog
                .entries()
                .iter()
                .find(|entry| {
                    entry.project_id.as_deref() == Some(project) && native_id(entry) == Some(id)
                })
                .ok_or(
                    "Latest native checkpoint changed before retention; earlier versions retained",
                )
        })
        .transpose()?;
    let mut remaining = keep.saturating_sub(usize::from(protected.is_some()));
    for entry in catalog
        .entries()
        .iter()
        .filter(|entry| entry.project_id.as_deref() == Some(project))
    {
        if protected.is_some_and(|saved| saved.snapshot == entry.snapshot) {
            continue;
        }
        if remaining != 0 {
            remaining -= 1;
            continue;
        }
        check()?;
        super::discard_from_runtime(workspace, runtime, &entry.snapshot, check)?;
    }
    check()?;
    if runtime.begin_existing()?.collect_sources(check).is_err() {
        // Cleanup refusal does not withhold a valid checkpoint. Uncertain
        // mutation still taints the actual service and fails this admission.
        check()?;
        runtime.begin_existing()?.cleanup_check()?;
    }
    Ok(())
}

pub(in crate::recovery) fn discard_project(
    workspace: &workspace::Registry,
    project: &str,
    saved: &Saved,
) -> Result<(), String> {
    let checkpoint = begin_checkpoint(&saved.runtime)?;
    let check = || {
        foreground_idle(&checkpoint)?;
        check_runtime(workspace, &saved.runtime)
    };
    check()?;
    let catalog = complete_catalog(workspace, &check)?;
    prune(
        workspace,
        &saved.runtime,
        &catalog,
        project,
        0,
        None,
        &check,
    )
}

pub(in crate::recovery) fn admit_resident(
    workspace: &workspace::Registry,
    dir: &Path,
    project: &str,
    bytes: u64,
) -> Result<(), String> {
    let runtime = workspace
        .try_lock()
        .map_err(|_| "Workspace is busy; recovery deferred")?
        .source_runtime
        .clone();
    let checkpoint = runtime.as_deref().map(begin_checkpoint).transpose()?;
    let catalog = complete_catalog(workspace, &|| {
        checkpoint.as_ref().map_or(Ok(()), foreground_idle)
    })?;
    drop(checkpoint);
    admit(&catalog, dir, project, bytes, false)
}
fn admit(
    catalog: &Catalog,
    dir: &Path,
    project: &str,
    bytes: u64,
    native: bool,
) -> Result<(), String> {
    let resident = super::super::complete_resident_catalog(dir)?;
    let legacy = super::super::legacy(dir);
    let mut projects = [None; MAX_PROJECTS];
    let mut count = 0;
    let mut admit_project = |id| -> Result<(), String> {
        if !projects[..count].contains(&Some(id)) {
            if count == MAX_PROJECTS {
                return Err(
                    "Recovery storage is full (64 projects); existing recoveries retained".into(),
                );
            }
            projects[count] = Some(id);
            count += 1;
        }
        Ok(())
    };
    for id in catalog
        .entries()
        .iter()
        .filter_map(|entry| entry.project_id.as_deref())
        .chain(resident.iter().map(|entry| entry.project_id.as_str()))
    {
        admit_project(id)?;
    }
    for (_, path) in &legacy {
        let id = path
            .file_stem()
            .and_then(|name| name.to_str())
            .and_then(|name| name.strip_prefix("recovery-"))
            .ok_or("Legacy recovery identity changed")?;
        admit_project(id)?;
    }
    admit_project(project)?;
    let mut retained = 0;
    let mut total = bytes;
    // Keep two earlier versions plus the new one, as in the PSD policy. The
    // shared source-disk budget separately bounds actual native pixel stores.
    for entry in catalog.entries() {
        if native && entry.project_id.as_deref() == Some(project) {
            retained += 1;
            if retained >= VERSIONS_PER_PROJECT {
                continue;
            }
        }
        total = total
            .checked_add(entry.bytes.ok_or("Native marker length unavailable")?)
            .ok_or("Recovery byte total overflow")?;
    }
    let mut resident_retained = 0;
    for bytes in resident
        .iter()
        .filter(|entry| {
            if !native && entry.project_id == project {
                resident_retained += 1;
                resident_retained < VERSIONS_PER_PROJECT
            } else {
                true
            }
        })
        .map(|entry| entry.bytes)
        .chain(
            legacy
                .iter()
                .map(|(_, path)| std::fs::metadata(path).map(|m| m.len()).unwrap_or(u64::MAX)),
        )
    {
        total = total
            .checked_add(bytes)
            .ok_or("Recovery byte total overflow")?;
    }
    if total > MAX_BYTES {
        return Err(
            "Recovery storage is full (1 GiB); save work or explicitly discard old recoveries"
                .into(),
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(project: String, bytes: u64) -> NativeSnapshot {
        NativeSnapshot {
            snapshot: format!("native:{}", uuid::Uuid::new_v4()),
            project_id: Some(project),
            document_id: Some(uuid::Uuid::new_v4().to_string()),
            revision: Some(0),
            display_name: "Policy arithmetic fixture".into(),
            bit_depth: Some(16),
            timestamp_ms: Some(0),
            bytes: Some(bytes),
            read_only: Some(false),
            error: None,
        }
    }
    #[test]
    fn combined_storage_policy_counts_projects_once_and_checks_both_formats() {
        let fixture = crate::engine::native::worker::tests::Fixture::new();
        let project = uuid::Uuid::new_v4().to_string();
        let document = engine::Document::new_depth(2, 1, 16).unwrap();
        let bytes = psd::encode(&document).unwrap();
        super::super::super::persist(fixture.path(), &project, &document, None, &bytes, |_| Ok(()))
            .unwrap();
        let mut catalog = Catalog::default();
        catalog.entries.push(row(project.clone(), 4096));
        for _ in 1..MAX_PROJECTS {
            catalog
                .entries
                .push(row(uuid::Uuid::new_v4().to_string(), 4096));
        }
        // These rows exercise quota arithmetic only, never source/disposal authority.
        assert!(admit(
            &catalog,
            fixture.path(),
            &project,
            bytes.len() as u64,
            false
        )
        .is_ok());
        assert!(admit(
            &catalog,
            fixture.path(),
            &uuid::Uuid::new_v4().to_string(),
            4096,
            true
        )
        .unwrap_err()
        .contains("64 projects"));
        catalog.entries = (0..8)
            .map(|_| row(uuid::Uuid::new_v4().to_string(), MAX_BYTES / 8 - 1))
            .collect();
        assert!(admit(
            &catalog,
            fixture.path(),
            &project,
            bytes.len() as u64,
            false
        )
        .unwrap_err()
        .contains("1 GiB"));
        assert!(
            admit(&catalog, fixture.path(), &project, bytes.len() as u64, true)
                .unwrap_err()
                .contains("1 GiB")
        );
        catalog.entries = vec![row(project.clone(), u64::MAX)];
        assert!(admit(&catalog, fixture.path(), &project, 4096, true)
            .unwrap_err()
            .contains("overflow"));
        assert_eq!(super::super::super::catalog(fixture.path()).len(), 1);
    }
}
