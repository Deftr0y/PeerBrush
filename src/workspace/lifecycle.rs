//! Final source-bound exit validation. Reviewing projects never closes a subset.
use super::*;

#[derive(Clone, Debug, PartialEq)]
pub struct Reviewed {
    pub project: String,
    pub document: String,
    pub revision: u64,
    /// An explicit Don't Save decision applies only to this exact source revision.
    pub discard: bool,
}

/// Ask the native workspace to review one exact source. No closing or document
/// mutation occurs here; Save / Don't Save / Cancel remains a human UI decision.
pub fn request_close(
    root: &Shared,
    project: &str,
    document: &str,
    revision: u64,
    actor: &str,
) -> Result<(), String> {
    if actor != "human" {
        return Err("Only human input can request native close review".into());
    }
    let shared = get(root, project)?;
    let mut e = shared.lock().unwrap();
    guard(&e, document, revision)?;
    e.native_close_review = Some((document.into(), revision));
    Ok(())
}

/// Call only after the native user has reviewed every project. Takes existing
/// engines in the same order as transfers, then checks the registry at commit.
/// No engine is closed when any source, save, reservation or project set changed.
pub fn finish_exit(workspace: &Registry, reviewed: &[Reviewed]) -> Result<(), String> {
    finish(workspace, reviewed, false, true, false, |_| Ok(()))
}

/// Future verified native installers must use this path, never a discard-capable
/// ordinary close. The native caller must also reject unfinished UI gestures.
pub fn finish_update_exit(workspace: &Registry, reviewed: &[Reviewed]) -> Result<(), String> {
    finish_update_exit_with_profile(workspace, reviewed, |_| Ok(()))
}

/// Dormant final transaction for a future authenticated native exit caller.
/// The callback must authenticate/preserve the final profile, never invoke a
/// native continuation while shared locks are held. Observation is not consent.
pub(crate) fn finish_update_exit_with_profile(
    workspace: &Registry,
    reviewed: &[Reviewed],
    publish: impl FnOnce(&crate::tool_settings::Store) -> Result<(), String>,
) -> Result<(), String> {
    finish(workspace, reviewed, true, true, false, |tools| {
        publish(tools.ok_or("Tool preference writer authority is unavailable")?)
    })
}

/// Publish a pre-native-start checkpoint under the same complete workspace,
/// source, reservation and file-operation validation as atomic update exit.
/// The owning UI thread also serializes direct native preference controls.
/// Publication failure and success both leave every project open.
#[cfg(any(target_os = "macos", test))]
pub(crate) fn publish_update_checkpoint(
    workspace: &Registry,
    reviewed: &[Reviewed],
    publish: impl FnOnce(&crate::tool_settings::Store) -> Result<(), String>,
) -> Result<(), String> {
    finish(workspace, reviewed, true, false, false, |tools| {
        publish(tools.ok_or("Tool preference writer authority is unavailable")?)
    })
}

fn update_project_ready(e: &Engine) -> Result<(), String> {
    if e.doc.revision != e.saved_revision
        || crate::file_safety::source_conflict(e.path.as_deref(), e.file_version)
    {
        return Err(format!(
            "Save {} before restarting for an update",
            e.doc.name
        ));
    }
    if e.file_operations.load(std::sync::atomic::Ordering::Acquire) != 0
        || e.loading.is_some()
        || e.code_states()
            .as_array()
            .is_some_and(|jobs| jobs.iter().any(|job| job["running"] == true))
    {
        return Err(
            "Finish the active file, loading or code operation before restarting for an update"
                .into(),
        );
    }
    Ok(())
}

/// Read-only readiness inspection. The source-bound atomic exit checks again.
pub fn update_readiness(workspace: &Registry) -> Result<Vec<Reviewed>, String> {
    let mut reviewed = vec![];
    for (project, _, shared) in entries_in(workspace) {
        let e = shared
            .try_lock()
            .map_err(|_| "A project is still working")?;
        e.ensure_open()?;
        update_project_ready(&e)?;
        reviewed.push(Reviewed {
            project,
            document: e.doc.id.clone(),
            revision: e.doc.revision,
            discard: false,
        });
    }
    let state = workspace.lock().unwrap();
    if state.exiting
        || state
            .file_operations
            .load(std::sync::atomic::Ordering::Acquire)
            != 0
    {
        return Err(
            "Finish the current workspace/recovery operation before restarting for an update"
                .into(),
        );
    }
    state
        .tool_settings
        .try_lock()
        .map_err(|_| "Tool preferences are still working")?
        .update_checkpoint_ready()?;
    Ok(reviewed)
}

// shortcut: update restart admits only the untouched blank workspace. Persisting
// arbitrary live undo/redo across process replacement needs a separate compatible
// snapshot format; never silently erase that history just because files are saved.
#[cfg(any(windows, test))]
fn windows_blank_workspace<'a>(engines: impl Iterator<Item = &'a Engine>) -> Result<(), String> {
    let engines: Vec<_> = engines.collect();
    let refused = || {
        "Close your projects before installing a Windows update; open documents and undo history are retained.".to_string()
    };
    if engines.len() != 1 {
        return Err(refused());
    }
    let e = engines[0];
    if e.path.is_some()
        || !e.undo.is_empty()
        || !e.redo.is_empty()
        || !e.changes.is_empty()
        || !e.leases.is_empty()
        || e.saved_revision != 0
    {
        return Err(refused());
    }
    let mut blank = Document::new(1024, 768)?;
    blank.id = e.doc.id.clone();
    if e.doc.layers.len() != 1 {
        return Err(refused());
    }
    blank.layers[0].id = e.doc.layers[0].id.clone();
    blank.layers[0].effect_key = e.doc.layers[0].effect_key.clone();
    if serde_json::to_value(&blank).map_err(|e| e.to_string())?
        != serde_json::to_value(&e.doc).map_err(|e| e.to_string())?
    {
        return Err(refused());
    }
    Ok(())
}
#[cfg(any(windows, test))]
pub(crate) fn windows_update_readiness(workspace: &Registry) -> Result<Vec<Reviewed>, String> {
    let reviewed = update_readiness(workspace)?;
    let entries = entries_in(workspace);
    let engines = entries
        .iter()
        .map(|(_, _, engine)| {
            engine
                .try_lock()
                .map_err(|_| "A project is still working".to_string())
        })
        .collect::<Result<Vec<_>, _>>()?;
    windows_blank_workspace(engines.iter().map(|e| &**e))?;
    Ok(reviewed)
}
#[cfg(any(windows, test))]
pub(crate) fn finish_windows_update_exit(
    workspace: &Registry,
    reviewed: &[Reviewed],
    publish: impl FnOnce(&crate::tool_settings::Store) -> Result<(), String>,
) -> Result<(), String> {
    finish(workspace, reviewed, true, true, true, |tools| {
        publish(tools.ok_or("Tool preference writer authority is unavailable")?)
    })
}

fn finish(
    workspace: &Registry,
    reviewed: &[Reviewed],
    update: bool,
    close: bool,
    blank_windows_workspace: bool,
    publish: impl FnOnce(Option<&crate::tool_settings::Store>) -> Result<(), String>,
) -> Result<(), String> {
    #[cfg(not(any(windows, test)))]
    let _ = blank_windows_workspace;
    let mut entries = entries_in(workspace);
    entries.sort_by(|a, b| a.0.cmp(&b.0));
    let mut reviewed: Vec<_> = reviewed.iter().collect();
    reviewed.sort_by(|a, b| a.project.cmp(&b.project));
    if entries.len() != reviewed.len()
        || entries
            .iter()
            .zip(&reviewed)
            .any(|((id, _, _), r)| id != &r.project)
        || reviewed.windows(2).any(|w| w[0].project == w[1].project)
    {
        return Err("The open projects changed. Review all projects again before exiting.".into());
    }
    let mut engines: Vec<_> = entries
        .iter()
        .map(|(_, _, shared)| {
            shared
                .try_lock()
                .map_err(|_| "A project is still working. All projects remain open.".to_string())
        })
        .collect::<Result<_, _>>()?;
    #[cfg(any(windows, test))]
    if blank_windows_workspace {
        windows_blank_workspace(engines.iter().map(|engine| &**engine))?;
    }
    for (e, r) in engines.iter_mut().zip(&reviewed) {
        guard(e, &r.document, r.revision)?;
        if update {
            if r.discard {
                return Err("Update restart cannot discard unsaved work".into());
            }
            update_project_ready(e)?;
        } else {
            file_operations_idle(&e.file_operations)?;
        }
        if !r.discard
            && (e.doc.revision != e.saved_revision
                || crate::file_safety::source_conflict(e.path.as_deref(), e.file_version))
        {
            return Err(format!(
                "{} still has unsaved changes. Save, Don't Save or Cancel before exiting.",
                e.doc.name
            ));
        }
        e.check(
            "human",
            &[Scope {
                target: None,
                rect: None,
            }],
        )?;
    }
    let mut registry = workspace.lock().unwrap();
    if update {
        if registry
            .file_operations
            .load(std::sync::atomic::Ordering::Acquire)
            != 0
        {
            return Err(
                "Finish the workspace/recovery operation before restarting for an update".into(),
            );
        }
    } else {
        file_operations_idle(&registry.file_operations)?;
    }
    let mut live: Vec<_> = registry.entries.iter().map(|p| p.id.as_str()).collect();
    live.sort_unstable();
    if registry.exiting
        || live.iter().zip(&reviewed).any(|(id, r)| *id != r.project)
        || live.len() != reviewed.len()
    {
        return Err("The workspace changed during exit review. All projects remain open.".into());
    }
    let mut tools = if update {
        Some(
            registry
                .tool_settings
                .try_lock()
                .map_err(|_| "Tool preferences are still working")?,
        )
    } else {
        None
    };
    if let Some(tools) = &tools {
        tools.update_checkpoint_ready()?;
    }
    publish(tools.as_deref())?;
    if close {
        if let Some(tools) = tools.as_deref_mut() {
            tools.commit_update_exit();
        }
    }
    drop(tools);
    if !close {
        return Ok(());
    }
    registry.exiting = true;
    for e in &mut engines {
        if let Some(control) = e.loading.take() {
            control.cancel();
        }
        e.closed = true;
        e.release_file_ownership();
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn windows_restart_refuses_saved_projects_and_preserves_actual_history() {
        let shared = Arc::new(Mutex::new(Engine::new()));
        let workspace = attach(&shared);
        windows_update_readiness(&workspace).unwrap();
        {
            let mut e = shared.lock().unwrap();
            let layer = e.doc.layers[0].id.clone();
            e.edit(
                "human",
                &[json!({"op":"fill","layer":layer,"rect":[1,1,2,2],"color":[1,2,3,255]})],
                None,
                None,
                "Keep actual history",
            )
            .unwrap();
            e.saved_revision = e.doc.revision;
        }
        let before = shared.lock().unwrap().doc.revision;
        assert!(windows_update_readiness(&workspace).is_err());
        let reviewed = update_readiness(&workspace).unwrap();
        assert!(
            finish_windows_update_exit(&workspace, &reviewed, |_| panic!(
                "No final seal for an open project"
            ))
            .is_err()
        );
        let mut e = shared.lock().unwrap();
        assert!(!e.closed);
        assert_eq!(e.doc.revision, before);
        assert_eq!(e.undo.len(), 1);
        e.undo("human").unwrap();
        assert_eq!(e.redo.len(), 1);
    }
    #[test]
    fn checkpoint_publication_reuses_atomic_source_and_write_guards_without_closing() {
        let directory = std::env::temp_dir().join(format!(
            "peerbrush-checkpoint-workspace-{}",
            crate::engine::id()
        ));
        std::fs::create_dir(&directory).unwrap();
        {
            let mut engine = Engine::new();
            engine.doc = Document::new_depth(8, 6, 8).unwrap();
            let root = Arc::new(Mutex::new(engine));
            let workspace = attach(&root);
            let second = new_project(&root, 8, 6, 16, None).unwrap();
            let projects = [&root, &second];
            let paths: Vec<_> = [8, 16]
                .into_iter()
                .map(|depth| directory.join(format!("art-{depth}.psd")))
                .collect();
            for (shared, path) in projects.into_iter().zip(&paths) {
                {
                    let mut e = shared.lock().unwrap();
                    if e.doc.bit_depth == 16 {
                        e.doc.layers[0].pixels.set16(0, 0, [1001, 3003, 5007, 0]);
                    } else {
                        e.doc.layers[0].pixels.set(0, 0, [17, 93, 207, 0]);
                    }
                    let layer = e.doc.layers[0].id.clone();
                    e.edit("human", &[json!({"op":"fill","layer":layer,"rect":[1,1,3,3],"color":[41,83,127,173]})],None,None,"Keep real undo").unwrap();
                    e.edit(
                        "human",
                        &[json!({"op":"layer.update","layer":layer,"name":"Keep real redo"})],
                        None,
                        None,
                        "Keep real redo",
                    )
                    .unwrap();
                    e.undo("human").unwrap();
                    assert!(!e.undo.is_empty() && !e.redo.is_empty());
                    e.reserve("human", "Keep human ownership", vec![Scope::layer(&layer)])
                        .unwrap();
                }
                crate::server::save(shared, path).unwrap();
            }
            let snapshot = || {
                projects.iter().zip(&paths).map(|(shared,path)| {
                let e = shared.lock().unwrap();
                let history = |entries: &[crate::engine::History]| entries.iter().map(|h|json!({
                    "before":h.before,"after":h.after,"actor":h.actor,"label":h.label,"task":h.task,
                    "scopes":h.scopes,"gesture":h.gesture,"reverted":h.reverted
                })).collect::<Vec<_>>();
                assert!(!e.undo.is_empty() && !e.redo.is_empty() && !e.leases.is_empty());
                json!({"project":e.project_id,"document":e.doc,"undo":history(&e.undo),"redo":history(&e.redo),
                    "leases":e.leases,"saved":e.saved_revision,"path":e.path,
                    "closed":e.closed,"file_bytes":std::fs::read(path).unwrap(),
                    "owner_path":e.file_ownership.as_ref().map(|owner|&owner.path),
                    "owner_project":crate::file_ownership::local_project(path)})
            }).collect::<Vec<_>>()
            };
            let expected = snapshot();
            let owners: Vec<_> = projects
                .iter()
                .map(|shared| shared.lock().unwrap().file_ownership.clone().unwrap())
                .collect();
            let versions: Vec<_> = projects
                .iter()
                .map(|shared| shared.lock().unwrap().file_version)
                .collect();
            let unchanged = || {
                assert_eq!(snapshot(), expected);
                assert_eq!(entries_in(&workspace).len(), 2);
                assert!(!workspace.lock().unwrap().exiting);
                for (index, ((shared, path), owner)) in
                    projects.iter().zip(&paths).zip(&owners).enumerate()
                {
                    let e = shared.lock().unwrap();
                    assert_eq!(e.file_version, versions[index]);
                    assert!(Arc::ptr_eq(e.file_ownership.as_ref().unwrap(), owner));
                    assert!(
                        crate::file_ownership::Ownership::open(path, "unrelated-project").is_err()
                    );
                }
            };
            let reviewed = update_readiness(&workspace).unwrap();
            let published = std::cell::Cell::new(false);
            let tools = workspace.lock().unwrap().tool_settings.clone();
            {
                let mut store = tools.lock().unwrap();
                *store = crate::tool_settings::Store::load(directory.join("tool-settings.json"));
                let before = store.get("brush").unwrap().profile;
                let mut after = before.clone();
                after.settings.radius += 1.;
                store.human_delta("brush", &before, &after).unwrap();
            }
            assert!(update_readiness(&workspace).is_err());
            assert!(publish_update_checkpoint(&workspace, &reviewed, |_| {
                published.set(true);
                Ok(())
            })
            .is_err());
            assert!(!published.get());
            unchanged();
            tools.lock().unwrap().flush().unwrap();
            update_readiness(&workspace).unwrap();
            let operation = crate::server::FileOperation::begin_workspace(&workspace).unwrap();
            assert!(publish_update_checkpoint(&workspace, &reviewed, |_| {
                published.set(true);
                Ok(())
            })
            .is_err());
            assert!(!published.get());
            unchanged();
            drop(operation);
            assert_eq!(
                publish_update_checkpoint(&workspace, &reviewed, |_| Err(
                    "Durable publication failed".into()
                ))
                .unwrap_err(),
                "Durable publication failed"
            );
            unchanged();
            publish_update_checkpoint(&workspace, &reviewed, |_| {
                assert!(root.try_lock().is_err());
                assert!(second.try_lock().is_err());
                assert!(workspace.try_lock().is_err());
                assert!(tools.try_lock().is_err());
                published.set(true);
                Ok(())
            })
            .unwrap();
            assert!(published.get());
            unchanged();
        }
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn update_exit_keeps_original_operation_diagnostics_and_precedence_without_closing_projects() {
        for depth in [8, 16] {
            let mut engine = Engine::new();
            engine.doc = Document::new_depth(8, 6, depth).unwrap();
            let root = Arc::new(Mutex::new(engine));
            let workspace = attach(&root);
            let second = new_project(&root, 8, 6, depth, None).unwrap();
            let mut reviewed = update_readiness(&workspace).unwrap();
            let project = second.lock().unwrap().project_id.clone();
            let write = crate::server::FileOperation::begin(&second).unwrap();
            assert_eq!(
                finish_update_exit(&workspace, &reviewed).unwrap_err(),
                "Finish the active file, loading or code operation before restarting for an update"
            );
            assert_eq!(
                finish_exit(&workspace, &reviewed).unwrap_err(),
                "Finish the active file or workspace operation before closing projects"
            );
            let row = reviewed
                .iter_mut()
                .find(|row| row.project == project)
                .unwrap();
            row.discard = true;
            assert_eq!(
                finish_update_exit(&workspace, &reviewed).unwrap_err(),
                "Update restart cannot discard unsaved work"
            );
            reviewed
                .iter_mut()
                .find(|row| row.project == project)
                .unwrap()
                .discard = false;
            // Actual new work and an updated exact source review: Save must
            // retain priority over an in-flight project operation for updates.
            let name = {
                let mut e = second.lock().unwrap();
                let layer = e.doc.layers[0].id.clone();
                e.edit(
                    "human",
                    &[json!({"op":"layer.update","layer":layer,"name":"Human work"})],
                    None,
                    None,
                    "Keep human work",
                )
                .unwrap();
                assert_ne!(e.doc.revision, e.saved_revision);
                reviewed
                    .iter_mut()
                    .find(|row| row.project == project)
                    .unwrap()
                    .revision = e.doc.revision;
                e.doc.name.clone()
            };
            let snapshot = |shared: &Shared| {
                let e = shared.lock().unwrap();
                json!({"document":e.doc,"png":e.doc.export_png().unwrap(),
                    "undo":e.undo.len(),"redo":e.redo.len(),"saved":e.saved_revision,"closed":e.closed})
            };
            let before = snapshot(&second);
            assert_eq!(
                finish_update_exit(&workspace, &reviewed).unwrap_err(),
                format!("Save {name} before restarting for an update")
            );
            assert_eq!(snapshot(&second), before);
            assert!(!workspace.lock().unwrap().exiting);
            assert!(!root.lock().unwrap().closed);
            drop(write);

            // Keep the dirty project in the ordinary human path; a fresh saved
            // workspace exercises the original final workspace/recovery guard.
            let root = Arc::new(Mutex::new(Engine::new()));
            root.lock().unwrap().doc = Document::new_depth(8, 6, depth).unwrap();
            let workspace = attach(&root);
            let reviewed = update_readiness(&workspace).unwrap();
            let before = snapshot(&root);
            let write = crate::server::FileOperation::begin_workspace(&workspace).unwrap();
            assert_eq!(
                update_readiness(&workspace).unwrap_err(),
                "Finish the current workspace/recovery operation before restarting for an update"
            );
            assert_eq!(
                finish_update_exit(&workspace, &reviewed).unwrap_err(),
                "Finish the workspace/recovery operation before restarting for an update"
            );
            assert_eq!(
                finish_exit(&workspace, &reviewed).unwrap_err(),
                "Finish the active file or workspace operation before closing projects"
            );
            assert_eq!(snapshot(&root), before);
            assert!(!workspace.lock().unwrap().exiting);
            drop(write);
            finish_update_exit(&workspace, &reviewed).unwrap();
            assert!(root.lock().unwrap().closed);
        }
    }

    #[test]
    fn ordinary_close_and_exit_refuse_late_file_operations_without_closing_any_project() {
        for depth in [8, 16] {
            for discard in [false, true] {
                for exit in [false, true] {
                    let mut engine = Engine::new();
                    engine.doc = Document::new_depth(8, 6, depth).unwrap();
                    let root = Arc::new(Mutex::new(engine));
                    let workspace = attach(&root);
                    let second = new_project(&root, 8, 6, depth, None).unwrap();
                    let mut reviewed = vec![];
                    let mut before = vec![];
                    for shared in [&root, &second] {
                        let mut e = shared.lock().unwrap();
                        e.doc.layers[0]
                            .pixels
                            .set16(2, 3, [12001, 23003, 34007, 45009]);
                        if discard {
                            let layer = e.doc.layers[0].id.clone();
                            e.edit("human", &[json!({"op":"layer.update","layer":layer,"name":"Reviewed unsaved source"})], None, None, "Keep work").unwrap();
                        }
                        reviewed.push(Reviewed {
                            project: e.project_id.clone(),
                            document: e.doc.id.clone(),
                            revision: e.doc.revision,
                            discard,
                        });
                        before.push((e.doc.clone(), e.undo.len(), e.redo.len(), e.saved_revision));
                    }
                    let close = || {
                        if exit {
                            finish_exit(&workspace, &reviewed)
                        } else {
                            let row = &reviewed[1];
                            close_in(
                                &workspace,
                                &row.project,
                                &row.document,
                                row.revision,
                                row.discard,
                                "human",
                            )
                        }
                    };
                    // The operation starts after exact sources were reviewed, on
                    // the inactive project; both final paths must check again.
                    let write = crate::server::FileOperation::begin(&second).unwrap();
                    assert!(close().unwrap_err().contains("active file"));
                    drop(write);
                    let preference =
                        crate::server::FileOperation::begin_workspace(&workspace).unwrap();
                    assert!(close().unwrap_err().contains("active file"));
                    drop(preference);
                    assert!(!workspace.lock().unwrap().exiting);
                    assert_eq!(entries_in(&workspace).len(), 2);
                    for (shared, expected) in [&root, &second].into_iter().zip(&before) {
                        let e = shared.lock().unwrap();
                        assert!(!e.closed);
                        assert_eq!(
                            (
                                serde_json::to_value(&e.doc).unwrap(),
                                e.undo.len(),
                                e.redo.len(),
                                e.saved_revision,
                            ),
                            (
                                serde_json::to_value(&expected.0).unwrap(),
                                expected.1,
                                expected.2,
                                expected.3,
                            )
                        );
                    }
                    close().unwrap();
                    assert!(second.lock().unwrap().closed);
                    assert_eq!(root.lock().unwrap().closed, exit);
                }
            }
        }
    }
}
