//! Stable project engines. Registry locks never acquire existing engine locks.
pub mod lifecycle;
use crate::{
    engine::{self, Document, Engine, Scope},
    server::Shared,
};
use serde_json::{json, Value};
use std::sync::{Arc, Mutex, Weak};
fn registry_cache() -> &'static Mutex<std::collections::HashMap<usize, Weak<Mutex<Workspace>>>> {
    static CACHE: std::sync::OnceLock<
        Mutex<std::collections::HashMap<usize, Weak<Mutex<Workspace>>>>,
    > = std::sync::OnceLock::new();
    CACHE.get_or_init(Default::default)
}

#[derive(Clone)]
enum Handle {
    Root(Weak<Mutex<Engine>>),
    Owned(Shared),
}
impl Handle {
    fn get(&self) -> Option<Shared> {
        match self {
            Self::Root(root) => root.upgrade(),
            Self::Owned(shared) => Some(shared.clone()),
        }
    }
}
#[derive(Clone)]
struct Entry {
    id: String,
    name: String,
    handle: Handle,
}
pub struct Workspace {
    entries: Vec<Entry>,
    active: String,
    pub(crate) exiting: bool,
    pub brush_library: Arc<Mutex<crate::brush_library::Library>>,
}
pub type Registry = Arc<Mutex<Workspace>>;
pub const MAX_PROJECTS: usize = 16;

pub fn attach(root: &Shared) -> Registry {
    let key = Arc::as_ptr(root) as usize;
    if let Some(workspace) = registry_cache()
        .lock()
        .unwrap()
        .get(&key)
        .and_then(Weak::upgrade)
    {
        return workspace;
    }
    let mut e = root.lock().unwrap();
    if let Some(workspace) = e.workspace.as_ref().and_then(Weak::upgrade) {
        registry_cache()
            .lock()
            .unwrap()
            .insert(key, Arc::downgrade(&workspace));
        return workspace;
    }
    let workspace = Arc::new(Mutex::new(Workspace {
        entries: vec![Entry {
            id: e.project_id.clone(),
            name: e.doc.name.clone(),
            handle: Handle::Root(Arc::downgrade(root)),
        }],
        active: e.project_id.clone(),
        exiting: false,
        brush_library: e.brush_library.clone(),
    }));
    e.workspace = Some(Arc::downgrade(&workspace));
    e.workspace_owner = Some(workspace.clone());
    let mut cache = registry_cache().lock().unwrap();
    cache.retain(|_, workspace| workspace.strong_count() > 0);
    cache.insert(key, Arc::downgrade(&workspace));
    workspace
}
pub fn handles(root: &Shared) -> Vec<Shared> {
    handles_in(&attach(root))
}
pub fn handles_in(workspace: &Registry) -> Vec<Shared> {
    workspace
        .lock()
        .unwrap()
        .entries
        .iter()
        .filter_map(|p| p.handle.get())
        .collect()
}
/// UI can retain names while a background engine is busy; no existing engine is locked here.
pub fn entries_in(workspace: &Registry) -> Vec<(String, String, Shared)> {
    workspace
        .lock()
        .unwrap()
        .entries
        .iter()
        .filter_map(|p| {
            p.handle
                .get()
                .map(|shared| (p.id.clone(), p.name.clone(), shared))
        })
        .collect()
}
pub fn get(root: &Shared, id: &str) -> Result<Shared, String> {
    get_in(&attach(root), id)
}
pub fn get_in(workspace: &Registry, id: &str) -> Result<Shared, String> {
    let shared = workspace
        .lock()
        .unwrap()
        .entries
        .iter()
        .find(|p| p.id == id)
        .and_then(|p| p.handle.get())
        .ok_or("Missing or closed project ID")?;
    shared.lock().unwrap().ensure_open()?;
    Ok(shared)
}
pub fn active_id(root: &Shared) -> String {
    active_id_in(&attach(root))
}
pub fn active_id_in(workspace: &Registry) -> String {
    workspace.lock().unwrap().active.clone()
}
pub fn active(root: &Shared) -> Result<Shared, String> {
    get(root, &active_id(root))
}
pub fn mutating(method: &str, p: &Value) -> bool {
    match method {
        "observe" | "capabilities" | "brushes" => false,
        "task" => !matches!(p["action"].as_str(), Some("status" | "recovery")),
        "history" => !matches!(p["action"].as_str(), None | Some("list" | "inspect_task")),
        "proposal" => matches!(p["action"].as_str(), Some("create" | "accept" | "reject")),
        _ => true,
    }
}
pub fn validate_target(e: &Engine, p: &Value) -> Result<(), String> {
    e.ensure_open()?;
    if let Some(document) = p.get("document_id") {
        if document.as_str() != Some(&e.doc.id) {
            return Err("Project document changed; observe this project again".into());
        }
    }
    if p.get("project_id").is_some()
        && p["expected_revision"]
            .as_u64()
            .is_some_and(|r| r != e.doc.revision)
    {
        return Err("Targeted project revision changed; observe this project again".into());
    }
    Ok(())
}
pub fn route(root: &Shared, method: &str, p: &Value) -> Result<Shared, String> {
    let handles = handles(root);
    let explicit = p
        .get("project_id")
        .map(|v| v.as_str().ok_or("project_id must be text"))
        .transpose()?;
    let actor = p["actor"].as_str().unwrap_or("agent");
    if mutating(method, p) && actor != "human" && (explicit.is_some() || handles.len() > 1) {
        if explicit.is_none() {
            return Err("Multiple projects are open. Supply project_id, document_id and expected_revision from an observation".into());
        }
        if p["document_id"].as_str().is_none() || p["expected_revision"].as_u64().is_none() {
            return Err("Targeted AI changes need document_id and expected_revision from this project's observation".into());
        }
    }
    let shared = if let Some(id) = explicit {
        get(root, id)?
    } else {
        active(root)?
    };
    if mutating(method, p) {
        validate_target(&shared.lock().unwrap(), p)?;
    }
    Ok(shared)
}
pub fn select(root: &Shared, id: &str) -> Result<(), String> {
    select_in(&attach(root), id)
}
pub fn select_in(workspace: &Registry, id: &str) -> Result<(), String> {
    let mut workspace = workspace.lock().unwrap();
    if !workspace
        .entries
        .iter()
        .any(|p| p.id == id && p.handle.get().is_some())
        || workspace.exiting
    {
        return Err("Missing, closed or exiting project".into());
    }
    workspace.active = id.into();
    Ok(())
}
pub fn state(root: &Shared) -> Value {
    let active = active_id(root);
    let projects=handles(root).into_iter().filter_map(|shared| {
        let mut e=shared.lock().unwrap();e.expire();
        if e.closed {return None;}
        Some(json!({"project_id":e.project_id,"document_id":e.doc.id,"name":e.doc.name,"revision":e.doc.revision,"dirty":e.doc.revision!=e.saved_revision,"path":e.path,"bit_depth":e.doc.bit_depth,"read_only":e.doc.read_only,"loading":e.loading.as_ref().map(|c|c.status().stage),"ai_change":e.ai_change}))
    }).collect::<Vec<_>>();
    json!({"active_project_id":active,"projects":projects,"max_projects":MAX_PROJECTS})
}
/// Register only a successfully prepared project. Activation is conditional on the initiating tab.
pub fn register(
    root: &Shared,
    engine: Engine,
    activate_if: Option<&str>,
) -> Result<Shared, String> {
    let workspace = attach(root);
    register_in(&workspace, engine, activate_if)
}
pub fn register_in(
    workspace: &Registry,
    mut engine: Engine,
    activate_if: Option<&str>,
) -> Result<Shared, String> {
    engine.ensure_open()?;
    engine.brush_library = workspace.lock().unwrap().brush_library.clone();
    engine.workspace = Some(Arc::downgrade(workspace));
    engine.workspace_owner = None;
    // A runtime source identity distinguishes multiple opens of the same serialized PSD.
    engine.doc.id = engine::id();
    let id = engine.project_id.clone();
    let name = engine.doc.name.clone();
    let shared = Arc::new(Mutex::new(engine));
    let mut workspace = workspace.lock().unwrap();
    if workspace.exiting {
        return Err("The workspace is exiting; project was not opened".into());
    }
    if workspace.entries.len() >= MAX_PROJECTS {
        return Err("Close a project before opening another (16-project limit)".into());
    }
    if workspace.entries.iter().any(|p| p.id == id) {
        return Err("Project ID already exists".into());
    }
    workspace.entries.push(Entry {
        id: id.clone(),
        name,
        handle: Handle::Owned(shared.clone()),
    });
    if activate_if.is_some_and(|source| source == workspace.active) {
        workspace.active = id;
    }
    Ok(shared)
}
pub fn new_project(
    root: &Shared,
    width: u32,
    height: u32,
    depth: u16,
    activate_if: Option<&str>,
) -> Result<Shared, String> {
    let mut e = Engine::new();
    e.doc = Document::new_depth(width, height, depth)?;
    register(root, e, activate_if)
}
pub fn guard(e: &Engine, document: &str, revision: u64) -> Result<(), String> {
    e.ensure_open()?;
    if e.doc.id != document || e.doc.revision != revision {
        return Err("Project source changed; observe its document ID and revision again".into());
    }
    Ok(())
}
pub fn close(
    root: &Shared,
    id: &str,
    document: &str,
    revision: u64,
    discard: bool,
    actor: &str,
) -> Result<(), String> {
    let workspace = attach(root);
    close_in(&workspace, id, document, revision, discard, actor)
}
pub fn close_in(
    workspace: &Registry,
    id: &str,
    document: &str,
    revision: u64,
    discard: bool,
    actor: &str,
) -> Result<(), String> {
    let shared = entries_in(workspace)
        .into_iter()
        .find(|(project, _, _)| project == id)
        .map(|(_, _, shared)| shared)
        .ok_or("Missing or closed project ID")?;
    let mut e = shared
        .try_lock()
        .map_err(|_| "The project is still working. Try closing again when it finishes.")?;
    guard(&e, document, revision)?;
    if e.doc.revision != e.saved_revision && !discard {
        return Err("Unsaved project: save first, cancel, or explicitly choose Don't Save".into());
    }
    e.check(
        actor,
        &[Scope {
            target: None,
            rect: None,
        }],
    )?;
    let mut workspace_guard = workspace.lock().unwrap();
    if workspace_guard.exiting {
        return Err("Workspace is exiting".into());
    }
    if !workspace_guard.entries.iter().any(|p| p.id == id) {
        return Err("Missing or closed project ID".into());
    }
    if let Some(control) = e.loading.take() {
        control.cancel();
    }
    e.closed = true;
    e.doc = Document::new(1, 1).unwrap();
    e.saved_revision = 0;
    e.undo.clear();
    e.redo.clear();
    e.changes.clear();
    e.leases.clear();
    e.proposals.clear();
    e.recent_tasks.clear();
    e.path = None;
    e.file_version = None;
    e.ai_change = None;
    workspace_guard.entries.retain(|p| p.id != id);
    if workspace_guard.entries.is_empty() {
        let mut blank = Engine::new();
        blank.brush_library = e.brush_library.clone();
        blank.workspace = Some(Arc::downgrade(workspace));
        let id = blank.project_id.clone();
        workspace_guard.entries.push(Entry {
            id: id.clone(),
            name: blank.doc.name.clone(),
            handle: Handle::Owned(Arc::new(Mutex::new(blank))),
        });
        workspace_guard.active = id;
    } else if workspace_guard.active == id {
        workspace_guard.active = workspace_guard.entries[0].id.clone();
    }
    Ok(())
}

pub struct Transfer<'a> {
    pub source: &'a str,
    pub destination: &'a str,
    pub source_document: &'a str,
    pub destination_document: &'a str,
    pub source_revision: u64,
    pub destination_revision: u64,
    pub layers: &'a [String],
    pub target: &'a str,
    pub move_layers: bool,
    pub actor: &'a str,
    pub source_task: Option<&'a str>,
    pub destination_task: Option<&'a str>,
}
pub fn transfer(root: &Shared, request: &Transfer<'_>) -> Result<Value, String> {
    transfer_in(&attach(root), request)
}
pub fn transfer_in(workspace: &Registry, request: &Transfer<'_>) -> Result<Value, String> {
    if request.source == request.destination {
        return Err("Choose different source and destination projects".into());
    }
    let source = get_in(workspace, request.source)?;
    let destination = get_in(workspace, request.destination)?;
    // Every operation involving two existing engines takes the same lock order.
    if request.source < request.destination {
        let mut from = source.lock().unwrap();
        let mut to = destination.lock().unwrap();
        transfer_locked(&mut from, &mut to, request)
    } else {
        let mut to = destination.lock().unwrap();
        let mut from = source.lock().unwrap();
        transfer_locked(&mut from, &mut to, request)
    }
}
fn transfer_locked(from: &mut Engine, to: &mut Engine, r: &Transfer<'_>) -> Result<Value, String> {
    guard(from, r.source_document, r.source_revision)?;
    guard(to, r.destination_document, r.destination_revision)?;
    if from.doc.read_only {
        return Err("Protected Photoshop sources need an explicit compatible copy before transferring editable layers".into());
    }
    let snapshot = crate::layer_clipboard::copy(&from.doc, r.layers)?;
    let cut = if r.move_layers {
        Some(crate::layer_clipboard::cut_commands(&from.doc, r.layers)?)
    } else {
        None
    };
    let before_from = from.clone();
    let before_to = to.clone();
    let result = (|| {
        let pasted = to.paste_layers(
            r.actor,
            &snapshot,
            r.target,
            Some(r.destination_revision),
            r.destination_task,
        )?;
        if let Some(commands) = cut {
            from.edit(
                r.actor,
                &commands,
                Some(r.source_revision),
                r.source_task,
                "Move layers to another project",
            )?;
        }
        Ok(
            json!({"source_project_id":from.project_id,"destination_project_id":to.project_id,"source_revision":from.doc.revision,"destination_revision":to.doc.revision,"moved":r.move_layers,"created_roots":pasted["created_roots"],"created":pasted["created"]}),
        )
    })();
    if result.is_err() {
        *from = before_from;
        *to = before_to;
    }
    result
}
/// Actual destination pixels for a tab-hover preview, isolated from both authoritative histories.
pub fn transfer_preview(root: &Shared, r: &Transfer<'_>) -> Result<Document, String> {
    transfer_preview_in(&attach(root), r)
}
pub fn transfer_preview_in(workspace: &Registry, r: &Transfer<'_>) -> Result<Document, String> {
    if r.source == r.destination {
        return Err("Choose different source and destination projects".into());
    }
    let source = get_in(workspace, r.source)?;
    let destination = get_in(workspace, r.destination)?;
    let from = source.lock().unwrap();
    guard(&from, r.source_document, r.source_revision)?;
    if from.doc.read_only {
        return Err("Protected Photoshop sources need an explicit compatible copy before transferring editable layers".into());
    }
    let snapshot = crate::layer_clipboard::copy(&from.doc, r.layers)?;
    if r.move_layers {
        let commands = crate::layer_clipboard::cut_commands(&from.doc, r.layers)?;
        let mut source_draft = from.clone();
        source_draft.edit(
            r.actor,
            &commands,
            Some(r.source_revision),
            r.source_task,
            "Preview cross-project move",
        )?;
    }
    drop(from);
    let to = destination.lock().unwrap();
    guard(&to, r.destination_document, r.destination_revision)?;
    let mut draft = to.clone();
    drop(to);
    draft.paste_layers(
        r.actor,
        &snapshot,
        r.target,
        Some(r.destination_revision),
        r.destination_task,
    )?;
    Ok(draft.doc)
}
