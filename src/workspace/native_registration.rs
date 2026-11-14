//! Native tab metadata is prepared off-lock. Callers qualify actual sources and
//! hold their instance admission across the final memory-only registry update.
use super::*;
use crate::{
    engine::native::NativeDocument, loading::Control, psd::memory36::MetadataCharge,
    workspace::source_runtime::SourceRuntime,
};

pub(crate) struct Prepared {
    pub(crate) shared: Shared,
    runtime: Arc<SourceRuntime>,
    entry: Option<Entry>,
    active: Option<String>,
}
impl Prepared {
    pub(super) fn new(
        workspace: &Registry,
        runtime: &Arc<SourceRuntime>,
        document: NativeDocument,
        control: &Control,
    ) -> Result<Self, String> {
        control.check()?;
        document.validate_editable(&|| control.check())?;
        let document = document.for_runtime_registration(None, &|| control.check())?;
        Self::from_qualified_engine(
            workspace,
            runtime,
            Engine::from_native(document, 40)?,
            control,
        )
    }

    /// The caller retains the decoded file and any unpublished backing through
    /// registration. A compatibility result can carry read-only protection.
    pub(crate) fn from_qualified_engine(
        workspace: &Registry,
        runtime: &Arc<SourceRuntime>,
        mut engine: Engine,
        control: &Control,
    ) -> Result<Self, String> {
        control.check()?;
        engine.ensure_open()?;
        if engine.native_document().is_some_and(|document| {
            !document.budget().same_pool(&runtime.resident())
                || !document.disk_budget().same_pool(&runtime.disk())
        }) {
            return Err("Native tab belongs to another source workspace".into());
        }
        let (brush, tools, filters, exports) = {
            let state = workspace
                .try_lock()
                .map_err(|_| "Workspace is busy; tab retained")?;
            if state.exiting
                || !state
                    .source_runtime
                    .as_ref()
                    .is_some_and(|current| Arc::ptr_eq(current, runtime))
            {
                return Err("Native tab source service changed; tab retained".into());
            }
            if state.entries.len() >= MAX_PROJECTS {
                return Err("Close a project before opening another (16-project limit)".into());
            }
            (
                state.brush_library.clone(),
                state.tool_settings.clone(),
                state.filter_library.clone(),
                state.export_library.clone(),
            )
        };
        let view = engine::view::current(&engine);
        let bytes = view
            .name()
            .len()
            .checked_mul(2)
            .and_then(|n| n.checked_add(16384))
            .ok_or("Native tab registry admission overflow")?;
        let admission = MetadataCharge::reserve(&runtime.resident(), bytes, &|| control.check())?;
        let name = copy(view.name())?;
        engine.brush_library = brush;
        engine.tool_settings = tools;
        engine.filter_library = filters;
        engine.export_library = exports;
        engine.workspace = Some(Arc::downgrade(workspace));
        let id = copy(&engine.project_id)?;
        let active = copy(&engine.project_id)?;
        let shared = Arc::new(Mutex::new(engine));
        Ok(Self {
            entry: Some(Entry {
                id,
                name,
                handle: Handle::Owned(shared.clone()),
                _native_admission: Some(admission),
            }),
            shared,
            runtime: runtime.clone(),
            active: Some(active),
        })
    }
    /// The actual source/instance fence belongs to the caller. Borrowing self
    /// keeps refused document/runtime owners outside this registry lock.
    pub(super) fn publish(
        &mut self,
        workspace: &Registry,
        control: &Control,
        activate_if: Option<&str>,
    ) -> Result<(), String> {
        self.publish_with(workspace, control, activate_if, || ())
    }

    pub(crate) fn publish_with(
        &mut self,
        workspace: &Registry,
        control: &Control,
        activate_if: Option<&str>,
        before_register: impl FnOnce(),
    ) -> Result<(), String> {
        let mut state = workspace
            .try_lock()
            .map_err(|_| "Workspace is busy; tab retained")?;
        control.check()?;
        if state.exiting
            || !state
                .source_runtime
                .as_ref()
                .is_some_and(|current| Arc::ptr_eq(current, &self.runtime))
        {
            return Err("Native tab source service changed; tab retained".into());
        }
        let entry = self
            .entry
            .as_ref()
            .ok_or("Native tab was already registered")?;
        if state.entries.len() >= MAX_PROJECTS {
            return Err("Close a project before opening another (16-project limit)".into());
        }
        if state.entries.iter().any(|current| current.id == entry.id) {
            return Err("Native tab project identity already exists".into());
        }
        state
            .entries
            .try_reserve(1)
            .map_err(|_| "Native tab allocation refused")?;
        let activate = activate_if.is_some_and(|source| source == state.active);
        before_register();
        state.entries.push(self.entry.take().unwrap());
        if activate {
            state.active = self.active.take().unwrap();
        }
        Ok(())
    }
}

/// Shared native blank-tab preparation.
#[cfg(test)]
pub(crate) fn new_blank(
    workspace: &Registry,
    width: u32,
    height: u32,
    depth: u16,
    control: &Control,
    activate_if: Option<&str>,
) -> Result<Shared, String> {
    let runtime = workspace
        .lock()
        .unwrap()
        .source_runtime
        .clone()
        .ok_or("Native source service is not configured")?;
    new_blank_with(
        workspace,
        &runtime,
        (width, height, depth),
        control,
        activate_if,
        |_, _| Ok(()),
    )
    .map(|(shared, ())| shared)
}

pub(super) fn new_blank_with<T>(
    workspace: &Registry,
    runtime: &Arc<SourceRuntime>,
    dimensions: (u32, u32, u16),
    control: &Control,
    activate_if: Option<&str>,
    prepare_reply: impl FnOnce(&Shared, &source_runtime::SourceReadSession) -> Result<T, String>,
) -> Result<(Shared, T), String> {
    let _operation = crate::server::FileOperation::begin_workspace(workspace)?;
    control.begin()?;
    let (width, height, depth) = dimensions;
    let document = NativeDocument::new_blank(
        width,
        height,
        depth,
        &runtime.resident(),
        &runtime.disk(),
        &|| control.check(),
    )?;
    let mut prepared = Prepared::new(workspace, runtime, document, control)?;
    let scene = prepared
        .shared
        .lock()
        .unwrap()
        .capture_scene(None)?
        .into_native()?;
    // Known-zero scenes use actual memory-only instance admission, preserving
    // profile/source-namespace absence until a real source write is requested.
    let session = runtime.begin_read(&scene)?;
    session.check_scene(&scene)?;
    let reply = prepare_reply(&prepared.shared, &session)?;
    session.check_scene(&scene)?;
    session.with_live_fence(|| prepared.publish(workspace, control, activate_if))??;
    Ok((prepared.shared, reply))
}

fn copy(value: &str) -> Result<String, String> {
    let mut result = String::new();
    result
        .try_reserve_exact(value.len())
        .map_err(|_| "Native tab text allocation refused")?;
    if result.capacity() > value.len() {
        return Err("Native tab text exceeds admission".into());
    }
    result.push_str(value);
    Ok(result)
}

#[cfg(test)]
mod tests;
