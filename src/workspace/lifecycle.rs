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
        if !r.discard && e.doc.revision != e.saved_revision {
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
    }
    Ok(())
}
