//! Stable project engines. Registry locks never acquire existing engine locks.
pub mod lifecycle;
pub(crate) mod native_dispatch;
mod native_recovery;
pub(crate) mod native_registration;
mod native_transfer;
pub(crate) use native_transfer::NativeTransferPreview;
pub(crate) use native_recovery::register as register_native_recovery;
pub(crate) mod source_runtime;
use crate::{
    engine::{self, Document, Engine, Scope},
    server::Shared,
};
use serde_json::{json, Value};
use std::sync::{Arc, Mutex, Weak};
struct CachedWorkspace {
    root: Weak<Mutex<Engine>>,
    workspace: Weak<Mutex<Workspace>>,
}
fn registry_cache() -> &'static Mutex<std::collections::HashMap<usize, CachedWorkspace>> {
    static CACHE: std::sync::OnceLock<Mutex<std::collections::HashMap<usize, CachedWorkspace>>> =
        std::sync::OnceLock::new();
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
    _native_admission: Option<Arc<crate::psd::memory36::MetadataCharge>>,
}
pub struct Workspace {
    pub(crate) source_runtime: Option<Arc<source_runtime::SourceRuntime>>,
    pub(crate) native_proposals: Option<Arc<Mutex<crate::collaboration::native::Store>>>,
    pub(crate) native_proposals_closing: bool,
    pub(crate) activation: crate::activation::Queue,
    pub(crate) open_warning: Option<String>,
    entries: Vec<Entry>,
    active: String,
    pub(crate) exiting: bool,
    pub(crate) file_operations: Arc<std::sync::atomic::AtomicUsize>,
    pub brush_library: Arc<Mutex<crate::brush_library::Library>>,
    pub tool_settings: Arc<Mutex<crate::tool_settings::Store>>,
    pub filter_library: Arc<Mutex<crate::filter_library::Library>>,
    pub export_library: Arc<Mutex<crate::production_export::library::Library>>,
    pub recovery_dir: Option<std::path::PathBuf>,
    pub recovery_error: Option<String>,
    pub(crate) recovery_failures: std::collections::BTreeMap<String, String>,
    pub(crate) recovery_retired: Vec<String>,
}
pub type Registry = Arc<Mutex<Workspace>>;
pub const MAX_PROJECTS: usize = 16;

impl Workspace {
    /// Inspect selection while the caller holds the registry through capture dequeue.
    pub(crate) fn is_active_engine(&self, shared: &Shared) -> bool {
        self.entries.iter().any(|entry| {
            entry.id == self.active
                && entry
                    .handle
                    .get()
                    .is_some_and(|active| Arc::ptr_eq(&active, shared))
        })
    }
}

/// Queue native feedback without changing any document or presentation state.
pub(crate) fn duplicate_warning(
    workspace: &Registry,
    path: &std::path::Path,
    error: &str,
    focus: bool,
) {
    if !error.starts_with("File already open") {
        return;
    }
    let owner = crate::file_ownership::local_project(path);
    let mut w = workspace.lock().unwrap();
    let existing = owner.and_then(|id| {
        w.entries
            .iter()
            .find(|p| p.id == id)
            .map(|p| (p.id.clone(), p.name.clone()))
    });
    let detail = if let Some((id, name)) = existing {
        if focus {
            w.active = id;
        }
        format!("{} is already open in project {name}. Its unsaved work, selection, view and history are preserved.", path.file_name().unwrap_or_default().to_string_lossy())
    } else {
        error.to_string()
    };
    w.open_warning = Some(detail);
}

pub fn attach(root: &Shared) -> Registry {
    let key = Arc::as_ptr(root) as usize;
    if let Some(workspace) = registry_cache()
        .lock()
        .unwrap()
        .get(&key)
        .filter(|cached| cached.root.ptr_eq(&Arc::downgrade(root)))
        .and_then(|cached| cached.workspace.upgrade())
    {
        return workspace;
    }
    let mut e = root.lock().unwrap();
    if let Some(workspace) = e.workspace.as_ref().and_then(Weak::upgrade) {
        registry_cache().lock().unwrap().insert(
            key,
            CachedWorkspace {
                root: Arc::downgrade(root),
                workspace: Arc::downgrade(&workspace),
            },
        );
        return workspace;
    }
    let workspace = Arc::new(Mutex::new(Workspace {
        source_runtime: None,
        native_proposals: None,
        native_proposals_closing: false,
        activation: Default::default(),
        entries: vec![Entry {
            id: e.project_id.clone(),
            name: engine::view::current(&e).name().to_owned(),
            handle: Handle::Root(Arc::downgrade(root)),
            _native_admission: None,
        }],
        active: e.project_id.clone(),
        exiting: false,
        file_operations: Default::default(),
        brush_library: e.brush_library.clone(),
        tool_settings: e.tool_settings.clone(),
        filter_library: e.filter_library.clone(),
        export_library: e.export_library.clone(),
        recovery_dir: None,
        recovery_error: None,
        recovery_failures: Default::default(),
        recovery_retired: vec![],
        open_warning: None,
    }));
    e.workspace = Some(Arc::downgrade(&workspace));
    e.workspace_owner = Some(workspace.clone());
    let mut cache = registry_cache().lock().unwrap();
    // The weak root pins its allocation, preventing address reuse while the
    // cached registry survives in an engine snapshot after the root tab closes.
    cache.retain(|_, cached| cached.root.strong_count() > 0 && cached.workspace.strong_count() > 0);
    cache.insert(
        key,
        CachedWorkspace {
            root: Arc::downgrade(root),
            workspace: Arc::downgrade(&workspace),
        },
    );
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
        "observe" | "capabilities" | "brushes" | "fonts" => false,
        "task" => !matches!(p["action"].as_str(), Some("status" | "recovery")),
        "history" => !matches!(p["action"].as_str(), None | Some("list" | "inspect_task")),
        "proposal" => matches!(p["action"].as_str(), Some("create" | "accept" | "reject")),
        "code" => p["action"] == "start",
        _ => true,
    }
}
pub fn validate_target(e: &Engine, p: &Value) -> Result<(), String> {
    e.ensure_open()?;
    let view = engine::view::current(e);
    if let Some(document) = p.get("document_id") {
        if document.as_str() != Some(view.id()) {
            return Err("Project document changed; observe this project again".into());
        }
    }
    if p.get("project_id").is_some()
        && p["expected_revision"]
            .as_u64()
            .is_some_and(|r| r != view.revision())
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
/// Navigation only: preserve engine handles, source state and active project identity.
pub fn reorder_in(
    workspace: &Registry,
    expected_order: &[String],
    order: &[String],
) -> Result<(), String> {
    let mut workspace = workspace.lock().unwrap();
    if workspace.exiting {
        return Err("Workspace is exiting".into());
    }
    if !workspace
        .entries
        .iter()
        .map(|p| &p.id)
        .eq(expected_order.iter())
    {
        return Err("Open project order changed; start the tab drag again".into());
    }
    let expected: std::collections::HashSet<_> = expected_order.iter().collect();
    let requested: std::collections::HashSet<_> = order.iter().collect();
    if order.len() != expected_order.len()
        || requested.len() != order.len()
        || requested != expected
    {
        return Err("Tab order must contain every open project exactly once".into());
    }
    workspace
        .entries
        .sort_by_key(|p| order.iter().position(|id| id == &p.id).unwrap());
    Ok(())
}
pub fn state(root: &Shared) -> Value {
    let active = active_id(root);
    let projects=handles(root).into_iter().filter_map(|shared| {
        let mut e=shared.lock().unwrap();e.expire();
        if e.closed {return None;}
        let view = engine::view::current(&e);
        Some(json!({
            "project_id":e.project_id, "document_id":view.id(), "name":view.name(),
            "revision":view.revision(), "dirty":view.revision()!=e.saved_revision, "path":e.path,
            "bit_depth":view.depth(), "read_only":view.read_only(), "ai_change":e.ai_change,
            "loading":e.loading.as_ref().map(|control| control.status().stage),
            "saving":e.saving.as_ref().map(|control| control.status()),
        }))
    }).collect::<Vec<_>>();
    let error = attach(root).lock().unwrap().recovery_error.clone();
    json!({"active_project_id":active,"projects":projects,"max_projects":MAX_PROJECTS,"autosave_error":error})
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
    engine.tool_settings = workspace.lock().unwrap().tool_settings.clone();
    engine.filter_library = workspace.lock().unwrap().filter_library.clone();
    engine.export_library = workspace.lock().unwrap().export_library.clone();
    engine.workspace = Some(Arc::downgrade(workspace));
    engine.workspace_owner = None;
    // A runtime source identity distinguishes multiple opens of the same serialized PSD.
    engine.authority.resident_state_mut()?.document.id = engine::id();
    let id = engine.project_id.clone();
    let name = engine.authority.resident_state()?.document.name.clone();
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
        _native_admission: None,
    });
    if activate_if.is_some_and(|source| source == workspace.active) {
        workspace.active = id;
    }
    Ok(shared)
}
/// Move the independently opened project, including its file ownership, into its tab.
pub(crate) fn register_opened(
    workspace: &Registry,
    prepared: Shared,
    activate_if: Option<&str>,
) -> Result<Shared, String> {
    let engine = Arc::try_unwrap(prepared)
        .map_err(|_| "Opened project still has an active worker")?
        .into_inner()
        .map_err(|_| "Opened project worker failed")?;
    register_in(workspace, engine, activate_if)
}
pub fn new_project(
    root: &Shared,
    width: u32,
    height: u32,
    depth: u16,
    activate_if: Option<&str>,
) -> Result<Shared, String> {
    new_project_with(root, width, height, depth, activate_if, |_, _| Ok(()))
        .map(|(shared, ())| shared)
}

pub(crate) fn new_project_with<T>(
    initiating: &Shared,
    width: u32,
    height: u32,
    depth: u16,
    activate_if: Option<&str>,
    prepare_reply: impl FnOnce(&Shared, Option<&source_runtime::SourceReadSession>) -> Result<T, String>,
) -> Result<(Shared, T), String> {
    let (workspace, native_source) = {
        let engine = initiating.lock().unwrap();
        engine.ensure_open()?;
        (
            engine.workspace.as_ref().and_then(Weak::upgrade),
            engine.native_document().is_some(),
        )
    };
    let workspace = match workspace {
        Some(workspace) => workspace,
        None if native_source => return Err("Native project has no configured workspace".into()),
        None => attach(initiating),
    };
    let runtime = workspace.lock().unwrap().source_runtime.clone();
    if let Some(runtime) = runtime {
        return native_registration::new_blank_with(
            &workspace,
            &runtime,
            (width, height, depth),
            &crate::loading::Control::default(),
            activate_if,
            |shared, session| prepare_reply(shared, Some(session)),
        );
    }
    if native_source {
        return Err("Native source service is not configured".into());
    }
    let engine = Engine::from_document(Document::new_depth(width, height, depth)?);
    let shared = register_in(&workspace, engine, activate_if)?;
    let reply = prepare_reply(&shared, None)?;
    Ok((shared, reply))
}
pub fn guard(e: &Engine, document: &str, revision: u64) -> Result<(), String> {
    e.ensure_open()?;
    let view = engine::view::current(e);
    if view.id() != document || view.revision() != revision {
        return Err("Project source changed; observe its document ID and revision again".into());
    }
    Ok(())
}
fn file_operations_idle(counter: &std::sync::atomic::AtomicUsize) -> Result<(), String> {
    if counter.load(std::sync::atomic::Ordering::Acquire) != 0 {
        return Err("Finish the active file or workspace operation before closing projects".into());
    }
    Ok(())
}
fn needs_save(e: &Engine) -> bool {
    engine::view::current(e).revision() != e.saved_revision
        || crate::file_safety::source_conflict(e.path.as_deref(), e.file_version)
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
    file_operations_idle(&e.file_operations)?;
    if !discard && needs_save(&e) {
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
    file_operations_idle(&workspace_guard.file_operations)?;
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
    e.release_file_ownership();
    // Closed handles must see an empty document immediately. Release the old
    // pixels, histories and draft owners after releasing the workspace and project locks.
    let retired = (
        std::mem::replace(
            &mut e.authority,
            crate::engine::authority::DocumentAuthority::from_resident(
                Document::new(1, 1).unwrap(),
            ),
        ),
        std::mem::take(&mut e.proposals),
    );
    e.saved_revision = 0;
    e.changes.clear();
    e.leases.clear();
    e.recent_tasks.clear();
    e.path = None;
    e.file_version = None;
    e.ai_change = None;
    workspace_guard.entries.retain(|p| p.id != id);
    workspace_guard.recovery_retired.push(id.to_owned());
    if workspace_guard.entries.is_empty() {
        let mut blank = Engine::new();
        blank.brush_library = e.brush_library.clone();
        blank.tool_settings = e.tool_settings.clone();
        blank.filter_library = e.filter_library.clone();
        blank.export_library = e.export_library.clone();
        blank.workspace = Some(Arc::downgrade(workspace));
        let id = blank.project_id.clone();
        workspace_guard.entries.push(Entry {
            id: id.clone(),
            name: blank.authority.resident_state()?.document.name.clone(),
            handle: Handle::Owned(Arc::new(Mutex::new(blank))),
            _native_admission: None,
        });
        workspace_guard.active = id;
    } else if workspace_guard.active == id {
        workspace_guard.active = workspace_guard.entries[0].id.clone();
    }
    drop(workspace_guard);
    drop(e);
    drop(retired);
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
struct TransferPair {
    source: Shared,
    destination: Shared,
    source_first: bool,
}
impl TransferPair {
    fn new(workspace: &Registry, request: &Transfer<'_>) -> Result<Self, String> {
        if request.source == request.destination {
            return Err("Choose different source and destination projects".into());
        }
        let source = get_in(workspace, request.source)?;
        let destination = get_in(workspace, request.destination)?;
        if Arc::ptr_eq(&source, &destination) {
            return Err("Choose different source and destination projects".into());
        }
        Ok(Self {
            source,
            destination,
            source_first: request.source < request.destination,
        })
    }
    fn with<T>(&self, edit: impl FnOnce(&mut Engine, &mut Engine) -> T) -> T {
        // Every operation involving two existing engines takes the same lock order.
        if self.source_first {
            let mut from = self.source.lock().unwrap();
            let mut to = self.destination.lock().unwrap();
            edit(&mut from, &mut to)
        } else {
            let mut to = self.destination.lock().unwrap();
            let mut from = self.source.lock().unwrap();
            edit(&mut from, &mut to)
        }
    }
}

pub fn transfer_in(workspace: &Registry, request: &Transfer<'_>) -> Result<Value, String> {
    enum Start {
        Complete(Value),
        NativeSource(native_transfer::Capture),
        NativeDestination(native_transfer::Destination),
    }
    let _operation = crate::server::FileOperation::begin_workspace(workspace)?;
    let pair = TransferPair::new(workspace, request)?;
    let start = pair.with(|from, to| {
        if let Some(capture) = native_transfer::Destination::capture(from, to, request)? {
            return Ok(Start::NativeDestination(capture));
        }
        match native_transfer::Capture::from_pair(from, to, request)? {
            Some(capture) => Ok(Start::NativeSource(capture)),
            None => transfer_locked(from, to, request).map(Start::Complete),
        }
    })?;
    match start {
        Start::Complete(reply) => Ok(reply),
        Start::NativeSource(capture) => capture
            .prepare(&pair, request, &|| Ok(()))?
            .commit(&pair, request),
        Start::NativeDestination(capture) => capture
            .prepare(&pair, request, &|| Ok(()))?
            .commit(&pair, request),
    }
}
fn transfer_locked(from: &mut Engine, to: &mut Engine, r: &Transfer<'_>) -> Result<Value, String> {
    guard(from, r.source_document, r.source_revision)?;
    guard(to, r.destination_document, r.destination_revision)?;
    if engine::view::current(from).read_only() {
        return Err("Protected Photoshop sources need an explicit compatible copy before transferring editable layers".into());
    }
    let snapshot = crate::layer_clipboard::copy(from.resident()?, r.layers)?;
    let cut = if r.move_layers {
        Some(crate::layer_clipboard::cut_commands(
            from.resident()?,
            r.layers,
        )?)
    } else {
        None
    };
    let before_to = to.resident_checkpoint()?;
    let pasted = to.paste_layers(
        r.actor,
        &snapshot,
        r.target,
        Some(r.destination_revision),
        r.destination_task,
    )?;
    if let Some(commands) = cut {
        if let Err(reason) = from.edit(
            r.actor,
            &commands,
            Some(r.source_revision),
            r.source_task,
            "Move layers to another project",
        ) {
            before_to.restore(to)?;
            return Err(reason);
        }
    }
    Ok(transfer_reply(from, to, r, &pasted))
}

fn transfer_reply(from: &Engine, to: &Engine, r: &Transfer<'_>, pasted: &Value) -> Value {
    json!({"source_project_id":from.project_id,"destination_project_id":to.project_id,"source_revision":engine::view::current(from).revision(),"destination_revision":engine::view::current(to).revision(),"moved":r.move_layers,"created_roots":pasted["created_roots"],"created":pasted["created"]})
}
/// Actual destination pixels for a tab-hover preview, isolated from both authoritative histories.
pub fn transfer_preview(root: &Shared, r: &Transfer<'_>) -> Result<Document, String> {
    transfer_preview_in(&attach(root), r)
}
pub fn transfer_preview_in(workspace: &Registry, r: &Transfer<'_>) -> Result<Document, String> {
    let _operation = crate::server::FileOperation::begin_workspace(workspace)?;
    let pair = TransferPair::new(workspace, r)?;
    preview_resident_destination(&pair, r, &|| Ok(()))
}

fn preview_resident_destination(
    pair: &TransferPair,
    r: &Transfer<'_>,
    check: &dyn Fn() -> Result<(), String>,
) -> Result<Document, String> {
    check()?;
    if let Some(capture) = pair.with(|from, to| native_transfer::Capture::from_pair(from, to, r))? {
        return capture.prepare(pair, r, check)?.preview(pair, r, check);
    }
    let from = pair.source.lock().unwrap();
    guard(&from, r.source_document, r.source_revision)?;
    if engine::view::current(&from).read_only() {
        return Err("Protected Photoshop sources need an explicit compatible copy before transferring editable layers".into());
    }
    let snapshot = crate::layer_clipboard::copy(from.resident()?, r.layers)?;
    let cut = if r.move_layers {
        Some((
            crate::layer_clipboard::cut_commands(from.resident()?, r.layers)?,
            from.resident_draft()?,
        ))
    } else {
        None
    };
    drop(from);
    check()?;
    if let Some((commands, mut source_draft)) = cut {
        source_draft.edit(
            r.actor,
            &commands,
            Some(r.source_revision),
            r.source_task,
            "Preview cross-project move",
        )?;
    }
    let to = pair.destination.lock().unwrap();
    guard(&to, r.destination_document, r.destination_revision)?;
    let mut draft = to.resident_draft()?;
    drop(to);
    check()?;
    draft.paste_layers(
        r.actor,
        &snapshot,
        r.target,
        Some(r.destination_revision),
        r.destination_task,
    )?;
    check()?;
    draft.into_resident_document()
}

pub(crate) struct TransferSummary {
    pub(crate) name: String,
    pub(crate) layers: usize,
    pub(crate) bit_depth: u16,
}
impl TransferSummary {
    fn from_view(view: engine::view::DocumentView<'_>) -> Self {
        Self { name: view.name().to_owned(), layers: view.layer_count(), bit_depth: view.depth() }
    }
}

enum TransferPixels {
    Resident(Vec<u8>),
    Native(crate::large_document::PinnedBytes),
}

pub(crate) struct TransferImage {
    pub(crate) summary: TransferSummary,
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) rect: [i32; 4],
    pixels: TransferPixels,
}
impl TransferImage {
    pub(crate) fn bytes(&self) -> &[u8] {
        match &self.pixels {
            TransferPixels::Resident(pixels) => pixels,
            TransferPixels::Native(pixels) => pixels,
        }
    }
}

pub(crate) fn render_transfer_in(
    workspace: &Registry,
    r: &Transfer<'_>,
    edge: u32,
    coarse: Option<u32>,
    check: &crate::render::Check<'_>,
    emit_coarse: impl FnMut(TransferImage),
) -> Result<TransferImage, String> {
    render_transfer_with(workspace, r, edge, coarse, check, |image| image,
        |preview| preview.render_image(), emit_coarse)
}

pub(crate) fn render_transfer_with<T>(
    workspace: &Registry,
    r: &Transfer<'_>,
    edge: u32,
    coarse: Option<u32>,
    check: &crate::render::Check<'_>,
    render_resident: impl Fn(TransferImage) -> T,
    render_native: impl Fn(NativeTransferPreview<'_>) -> Result<T, String>,
    mut emit_coarse: impl FnMut(T),
) -> Result<T, String> {
    check()?;
    let _operation = crate::server::FileOperation::begin_workspace(workspace)?;
    let pair = TransferPair::new(workspace, r)?;
    if let Some(capture) = pair.with(|from, to| native_transfer::Destination::capture(from, to, r))? {
        return capture
            .prepare(&pair, r, check)?
            .preview(&pair, r, edge, coarse, check, render_native, &mut emit_coarse);
    }
    let doc = preview_resident_destination(&pair, r, check)?;
    let render = |edge| {
        check()?;
        let (width, height, bytes, rect) = doc.preview_checked(None, edge, None, false, check)?;
        check()?;
        Ok(render_resident(TransferImage {
            summary: TransferSummary::from_view((&doc).into()), width, height, rect,
            pixels: TransferPixels::Resident(bytes),
        }))
    };
    if let Some(coarse) = coarse.filter(|&coarse| coarse < edge && doc.width.max(doc.height) > coarse) {
        if let Ok(frame) = render(coarse) {
            emit_coarse(frame);
        }
    }
    render(edge)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn refused_cross_project_move_restores_edit_state_without_replacing_live_owners() {
        for depth in [8, 16] {
            let mut document = Document::new_depth(8, 6, depth).unwrap();
            document.layers[0]
                .pixels
                .set16(1, 2, [12347, 23459, 34571, 61379]);
            let root = Arc::new(Mutex::new(Engine::from_document(document)));
            let registry = attach(&root);
            let destination = new_project(&root, 8, 6, depth, None).unwrap();
            let (source_project, source_document, source_revision, layer) = {
                let mut engine = root.lock().unwrap();
                let layer = engine
                    .resident_state()
                    .expect("resident fixture")
                    .document
                    .layers[0]
                    .id
                    .clone();
                engine
                    .reserve(
                        "other-agent",
                        "Protect the move source",
                        vec![Scope::layer(&layer)],
                    )
                    .unwrap();
                (
                    engine.project_id.clone(),
                    engine
                        .resident_state()
                        .expect("resident fixture")
                        .document
                        .id
                        .clone(),
                    engine
                        .resident_state()
                        .expect("resident fixture")
                        .document
                        .revision,
                    layer,
                )
            };
            let (
                destination_project,
                destination_document,
                destination_revision,
                target,
                task,
                io_counter,
            ) = {
                let mut engine = destination.lock().unwrap();
                let target = engine
                    .resident_state()
                    .expect("resident fixture")
                    .document
                    .layers[0]
                    .id
                    .clone();
                engine
                    .edit(
                        "agent",
                        &[json!({"op":"layer.update","layer":target,"name":"Before move"})],
                        None,
                        None,
                        "Existing edit",
                    )
                    .unwrap();
                engine.undo("human").unwrap();
                let task = engine
                    .reserve("agent", "Destination task", vec![Scope::layer(&target)])
                    .unwrap();
                engine
                    .leases
                    .iter_mut()
                    .find(|lease| lease.id == task.id)
                    .unwrap()
                    .expires = engine::now() + 120;
                (
                    engine.project_id.clone(),
                    engine
                        .resident_state()
                        .expect("resident fixture")
                        .document
                        .id
                        .clone(),
                    engine
                        .resident_state()
                        .expect("resident fixture")
                        .document
                        .revision,
                    target,
                    task.id,
                    engine.file_operations.clone(),
                )
            };
            let proposal = crate::collaboration::propose(
                &destination,
                "agent",
                &json!({
                    "document_id":destination_document,"expected_revision":destination_revision,
                    "commands":[{"op":"layer.update","layer":target,"opacity":0.5}],
                }),
            )
            .unwrap();
            let id = proposal["id"].as_str().unwrap();
            let snapshot = destination.lock().unwrap().proposal_snapshot(id).unwrap();
            let before = {
                let engine = destination.lock().unwrap();
                json!({"document":engine.resident_state().expect("resident fixture").document,"undo":engine.resident_state().expect("resident fixture").undo.len(),"redo":engine.resident_state().expect("resident fixture").redo.len(),"ai":engine.ai_change,"status":engine.status,"leases":engine.leases})
            };
            let operation = crate::server::FileOperation::track(io_counter.clone());
            let source_before = serde_json::to_value(
                &root
                    .lock()
                    .unwrap()
                    .resident_state()
                    .expect("resident fixture")
                    .document,
            )
            .unwrap();
            let layers = [layer];
            let error = transfer_in(
                &registry,
                &Transfer {
                    actor: "agent",
                    source: &source_project,
                    destination: &destination_project,
                    source_document: &source_document,
                    source_revision,
                    destination_document: &destination_document,
                    destination_revision,
                    layers: &layers,
                    target: &target,
                    move_layers: true,
                    source_task: None,
                    destination_task: Some(&task),
                },
            )
            .unwrap_err();
            assert!(error.contains("Reserved"));
            assert_eq!(
                serde_json::to_value(
                    &root
                        .lock()
                        .unwrap()
                        .resident_state()
                        .expect("resident fixture")
                        .document
                )
                .unwrap(),
                source_before
            );
            let mut engine = destination.lock().unwrap();
            assert_eq!(
                json!({"document":engine.resident_state().expect("resident fixture").document,"undo":engine.resident_state().expect("resident fixture").undo.len(),"redo":engine.resident_state().expect("resident fixture").redo.len(),"ai":engine.ai_change,"status":engine.status,"leases":engine.leases}),
                before
            );
            assert!(Arc::ptr_eq(
                &snapshot,
                &engine.proposal_snapshot(id).unwrap()
            ));
            assert!(Arc::ptr_eq(&io_counter, &engine.file_operations));
            assert_eq!(io_counter.load(std::sync::atomic::Ordering::Acquire), 1);
            drop(operation);
            assert_eq!(io_counter.load(std::sync::atomic::Ordering::Acquire), 0);
        }
    }
    #[test]
    fn resident_checkpoint_cannot_reacquire_a_task_that_ended_during_preparation() {
        let mut engine = Engine::new();
        let task = engine.reserve("agent", "End this task", vec![]).unwrap();
        let checkpoint = engine.resident_checkpoint().unwrap();
        engine.finish_task(&task.id, "taken_over");
        checkpoint.restore(&mut engine).unwrap();
        assert!(engine.leases.is_empty());
        assert!(!engine.status.starts_with("Reserved by"));
        assert_eq!(engine.task_recovery()["recent"][0]["status"], "taken_over");
    }
    #[test]
    fn cached_workspace_cannot_attach_a_different_root_after_close() {
        let old_root = Arc::new(Mutex::new(Engine::new()));
        let old_workspace = attach(&old_root);
        let (old_project, document, revision) = {
            let engine = old_root.lock().unwrap();
            (
                engine.project_id.clone(),
                engine
                    .resident_state()
                    .expect("resident fixture")
                    .document
                    .id
                    .clone(),
                engine
                    .resident_state()
                    .expect("resident fixture")
                    .document
                    .revision,
            )
        };
        let retained_workspace = old_workspace.clone();
        close(&old_root, &old_project, &document, revision, false, "human").unwrap();
        let old_identity = Arc::downgrade(&old_root);
        drop(old_root);
        assert!(old_identity.upgrade().is_none());
        // A retained registry keeps the closed workspace alive. Model
        // an address-only cache collision directly rather than depending on
        // platform allocator timing to reuse the former root's address.
        let fresh = Arc::new(Mutex::new(Engine::new()));
        let project = fresh.lock().unwrap().project_id.clone();
        registry_cache().lock().unwrap().insert(
            Arc::as_ptr(&fresh) as usize,
            CachedWorkspace {
                root: old_identity,
                workspace: Arc::downgrade(&old_workspace),
            },
        );
        let current = attach(&fresh);
        assert!(!Arc::ptr_eq(&current, &old_workspace));
        assert!(Arc::ptr_eq(&current, &attach(&fresh)));
        assert!(Arc::ptr_eq(&fresh, &get_in(&current, &project).unwrap()));
        assert_eq!(active_id(&fresh), project);
        assert!(get_in(&current, &old_project).is_err());
        assert!(get_in(&old_workspace, &project).is_err());
        drop(retained_workspace);
        assert!(Arc::ptr_eq(&fresh, &get_in(&current, &project).unwrap()));
    }
}
