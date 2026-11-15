//! Native checkpoint inspection and restoration through the configured source owner.
use super::*;
use crate::{large_document::ResidentBudget, psd::memory36::MetadataCharge};
#[path = "native/autosave.rs"]
mod autosave;
pub(super) use autosave::{admit_resident, checkpoint, discard_project, Saved, FOREGROUND_ACTIVE};

#[derive(Serialize)]
pub(crate) struct NativeSnapshot {
    pub snapshot: String,
    pub project_id: Option<String>,
    pub document_id: Option<String>,
    pub revision: Option<u64>,
    pub display_name: String,
    pub bit_depth: Option<u16>,
    pub timestamp_ms: Option<u64>,
    pub bytes: Option<u64>,
    pub read_only: Option<bool>,
    pub error: Option<String>,
}
#[derive(Default)]
pub(crate) struct Catalog {
    entries: Vec<NativeSnapshot>,
    error: Option<String>,
    resident: Option<ResidentBudget>,
    _admission: Option<Arc<MetadataCharge>>,
}
impl Catalog {
    pub(crate) fn entries(&self) -> &[NativeSnapshot] {
        &self.entries
    }
    pub(crate) fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }
    pub(crate) fn pool(&self) -> Option<&ResidentBudget> {
        self.resident.as_ref()
    }
    pub(super) fn unavailable() -> Self {
        Self { error: Some("Native recoveries could not be inspected. Refresh to retry; existing files are retained.".into()), ..Self::default() }
    }
}

pub(super) fn catalog(
    workspace: &workspace::Registry,
    check: &dyn Fn() -> Result<(), String>,
) -> Result<Catalog, String> {
    check()?;
    let runtime = workspace
        .try_lock()
        .map_err(|_| "Workspace is busy")?
        .source_runtime
        .clone();
    let Some(runtime) = runtime else {
        return Ok(Catalog::default());
    };
    let Some(session) = runtime.begin_existing_if_present()? else {
        return Ok(Catalog::default());
    };
    let ids = session.recovery_marker_ids()?;
    let count = ids.iter().count();
    if count == 0 {
        return Ok(Catalog::default());
    }
    let bytes = count
        .checked_mul(2 * std::mem::size_of::<NativeSnapshot>() + 4096)
        .and_then(|n| n.checked_add(256))
        .ok_or("Native recovery catalog footprint overflow")?;
    let admission = MetadataCharge::reserve(&session.resident(), bytes, check)?;
    let mut entries = Vec::new();
    entries
        .try_reserve_exact(count)
        .map_err(|_| "Native recovery catalog allocation refused")?;
    if entries.capacity() > count.saturating_mul(2) {
        return Err("Native recovery catalog capacity differs".into());
    }
    for id in ids.iter() {
        check()?;
        let bytes = session
            .open_recovery_marker(id)
            .ok()
            .map(|marker| marker.len());
        let mut entry = NativeSnapshot {
            snapshot: format!("native:{}", uuid::Uuid::from_bytes(id)),
            project_id: None,
            document_id: None,
            revision: None,
            display_name: "Unreadable native recovery".into(),
            bit_depth: None,
            timestamp_ms: None,
            bytes,
            read_only: None,
            error: None,
        };
        match backed::load(&session, id, limits(), check) {
            Ok(decoded) => {
                let doc = decoded.pending.metadata();
                entry.project_id = Some(uuid::Uuid::from_bytes(decoded.project).to_string());
                entry.document_id = Some(doc.id.clone());
                entry.revision = Some(doc.revision);
                entry.display_name = display_name(&doc.name)?;
                if matches!(doc.bit_depth, 8 | 16) {
                    entry.bit_depth = Some(doc.bit_depth);
                } else {
                    entry.error = Some(
                        "This checkpoint has unsupported precision; its files are retained.".into(),
                    );
                }
                entry.timestamp_ms = Some(decoded.timestamp);
                entry.read_only = Some(doc.read_only || doc.icc_profile.is_some());
            }
            Err(_) => {
                check()?;
                entry.error = Some("This checkpoint could not be inspected; its files are retained. Refresh to retry.".into());
            }
        }
        entries.push(entry);
    }
    entries.sort_unstable_by(|a, b| {
        b.timestamp_ms
            .cmp(&a.timestamp_ms)
            .then(b.revision.cmp(&a.revision))
            .then(a.snapshot.cmp(&b.snapshot))
    });
    session.check()?;
    check()?;
    Ok(Catalog {
        entries,
        error: None,
        resident: Some(session.resident()),
        _admission: Some(admission),
    })
}

fn display_name(value: &str) -> Result<String, String> {
    let mut output = String::new();
    output
        .try_reserve_exact(1027)
        .map_err(|_| "Recovery display name allocation refused")?;
    if output.capacity() > 2054 {
        return Err("Recovery display name exceeds admission".into());
    }
    let mut characters = value.chars();
    for _ in 0..256 {
        let Some(c) = characters.next() else {
            return Ok(output);
        };
        output.push(if c.is_control() { ' ' } else { c });
    }
    if characters.next().is_some() {
        output.push('…');
    }
    Ok(output)
}

fn limits() -> backed::Limits {
    backed::Limits {
        metadata: crate::psd::canonical36::Limits {
            bytes: crate::psd::canonical36::MAX_BYTES,
            nodes: 8_388_608,
            depth: 128,
        },
        refs: 1_048_576,
        stores: 4096,
    }
}

pub(crate) const DISCARD_AVAILABLE: bool = cfg!(windows);

pub(super) fn discard(workspace: &workspace::Registry, snapshot: &str) -> Result<(), String> {
    let runtime = workspace
        .try_lock()
        .map_err(|_| "Workspace is busy; checkpoint retained")?
        .source_runtime
        .clone()
        .ok_or("Native source service is not configured")?;
    discard_from_runtime(workspace, &runtime, snapshot, &|| Ok(()))
}
fn discard_from_runtime(
    workspace: &workspace::Registry,
    runtime: &Arc<workspace::source_runtime::SourceRuntime>,
    snapshot: &str,
    check: &dyn Fn() -> Result<(), String>,
) -> Result<(), String> {
    let id = *uuid::Uuid::parse_str(
        snapshot
            .strip_prefix("native:")
            .ok_or("Choose an explicit native checkpoint")?,
    )
    .map_err(|_| "Invalid native recovery identity")?
    .as_bytes();
    check()?;
    runtime
        .begin_existing()?
        .discard_checkpoint(id, limits(), &|| {
            check()?;
            let state = workspace
                .try_lock()
                .map_err(|_| "Workspace is busy; checkpoint retained")?;
            if state.exiting
                || !state
                    .source_runtime
                    .as_ref()
                    .is_some_and(|current| Arc::ptr_eq(current, runtime))
            {
                return Err("Native recovery source service changed; checkpoint retained".into());
            }
            Ok(())
        })
}

pub(super) fn restore(
    workspace: &workspace::Registry,
    snapshot: &str,
    control: &crate::loading::Control,
    initiating: Option<&str>,
) -> Result<server::Shared, String> {
    let id = *uuid::Uuid::parse_str(snapshot.strip_prefix("native:").unwrap_or(snapshot))
        .map_err(|_| "Invalid recovery snapshot identity")?
        .as_bytes();
    let _operation = server::FileOperation::begin_workspace(workspace)?;
    let runtime = workspace
        .try_lock()
        .map_err(|_| "Workspace is busy; recovery retained")?
        .source_runtime
        .clone()
        .ok_or("Recovery version no longer exists; refresh the list")?;
    control.begin()?;
    control.progress("Checking native recovery sources", 0, 1)?;
    let session = runtime.begin_existing()?;
    let decoded = backed::load(&session, id, limits(), &|| control.check())?;
    control.progress("Preparing native recovery tab", 1, 1)?;
    workspace::register_native_recovery(workspace, &session, decoded, control, initiating)
}
