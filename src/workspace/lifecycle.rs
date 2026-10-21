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
    finish(workspace, reviewed, false)
}

/// Future verified native installers must use this path, never a discard-capable
/// ordinary close. The native caller must also reject unfinished UI gestures.
pub fn finish_update_exit(workspace: &Registry, reviewed: &[Reviewed]) -> Result<(), String> {
    finish(workspace, reviewed, true)
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
    Ok(reviewed)
}

fn finish(workspace: &Registry, reviewed: &[Reviewed], update: bool) -> Result<(), String> {
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
    for (e, r) in engines.iter_mut().zip(&reviewed) {
        guard(e, &r.document, r.revision)?;
        file_operations_idle(&e.file_operations)?;
        if update {
            if r.discard {
                return Err("Update restart cannot discard unsaved work".into());
            }
            update_project_ready(e)?;
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
    file_operations_idle(&registry.file_operations)?;
    let mut live: Vec<_> = registry.entries.iter().map(|p| p.id.as_str()).collect();
    live.sort_unstable();
    if registry.exiting
        || live.iter().zip(&reviewed).any(|(id, r)| *id != r.project)
        || live.len() != reviewed.len()
    {
        return Err("The workspace changed during exit review. All projects remain open.".into());
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
                            (&e.doc, e.undo.len(), e.redo.len(), e.saved_revision),
                            (&expected.0, expected.1, expected.2, expected.3)
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
