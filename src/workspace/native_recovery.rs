//! Only a fully decoded checkpoint can enter this native registry path.
use super::*;
use crate::{
    engine::native::NativeDocument, loading::Control, recovery::backed::DecodedNativeRecovery,
    workspace::source_runtime::SourceSession,
};

pub(crate) fn register(
    workspace: &Registry,
    session: &SourceSession,
    mut decoded: DecodedNativeRecovery<'_>,
    control: &Control,
    activate_if: Option<&str>,
) -> Result<Shared, String> {
    control.check()?;
    decoded.marker.check_session(session)?;
    let runtime = workspace
        .try_lock()
        .map_err(|_| "Workspace is busy; recovery retained")?
        .source_runtime
        .clone()
        .ok_or("Native source service is not configured")?;
    if !session.uses_runtime(&runtime) {
        return Err("Recovery belongs to another source workspace; sources retained".into());
    }
    let document = NativeDocument::from_pending(decoded.pending)?;
    let mut prepared = native_registration::Prepared::new(workspace, &runtime, document, control)?;
    {
        let mut engine = prepared.shared.lock().unwrap();
        engine.saved_revision = u64::MAX;
        engine.status = "Recovered into an unsaved tab · choose Save As · recovery retained".into();
    }
    for source in &decoded.seals {
        session.check_owned_checkpoint_source(source, &|| control.check())?;
    }
    session.recheck_checkpoint(&mut decoded.marker, &|| control.check())?;
    // Source verification and refused owner destruction stay outside this fence.
    session.with_live_fence(|| prepared.publish(workspace, control, activate_if))??;
    Ok(prepared.shared)
}
