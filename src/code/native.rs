//! Child completion precedes the ordinary shared native publication path.
use super::*;
use crate::{
    engine::native::NativeDocument,
    server::code_feedback::{Completion, Preparation},
};
pub(super) fn ownership(source: &NativeDocument, p: &Value) -> Result<Ownership, String> {
    let script = p["script"]
        .as_str()
        .filter(|text| !text.trim().is_empty() && text.len() <= 65536)
        .ok_or("Script must contain 1–65536 UTF-8 bytes")?;
    let label = p["description"]
        .as_str()
        .filter(|text| !text.trim().is_empty() && text.len() <= 240)
        .ok_or("Describe what the code edits in 1–240 bytes")?;
    let actor = p["actor"]
        .as_str()
        .filter(|text| !text.is_empty() && text.len() <= 128)
        .ok_or("Code needs a named AI actor")?;
    let task = p["task"]
        .as_str()
        .filter(|text| !text.is_empty() && text.len() <= 128)
        .ok_or("Code requires an active owned task")?;
    let scopes = p["scopes"]
        .as_array()
        .filter(|scopes| !scopes.is_empty() && scopes.len() <= 16)
        .ok_or("Declare 1–16 code editing scopes")?;
    let targets = scopes.iter().try_fold(0usize, |sum, scope| {
        let size = match scope.get("target") {
            None | Some(Value::Null) => 0,
            Some(Value::String(target)) if target.len() <= 1024 => target.len(),
            _ => return Err("Invalid code scope target"),
        };
        sum.checked_add(size).ok_or("Code scope footprint overflow")
    })?;
    let bytes = [
        script.len(),
        label.len(),
        actor.len(),
        task.len(),
        source.metadata().id().len(),
        targets,
        scopes.len() * std::mem::size_of::<Scope>(),
    ]
    .into_iter()
    .try_fold(0usize, usize::checked_add)
    .and_then(|sum| sum.checked_mul(2))
    .and_then(|sum| sum.checked_add(STACK_BYTES + 32768))
    .ok_or("Code job footprint overflow")?;
    let pool = source.budget().clone();
    let charge = crate::psd::memory36::MetadataCharge::reserve(&pool, bytes, &|| Ok(()))?;
    Ok(Ownership {
        pool,
        _charge: charge,
    })
}

pub(super) fn apply(
    shared: &Shared,
    job: &Job,
    source: &NativeDocument,
    script: &str,
    edge: u32,
    started: Instant,
) -> Result<Completion, String> {
    job.check_worker(shared, source.into(), started)?;
    let feedback = Preparation::new(source, edge, &|| {
        job.check_worker(shared, source.into(), started)
    })?;
    let commands = worker::commands(shared, job, source, script, started, &|| Ok(()))?;
    // Retain actual returned-command admission through preparation, atomic
    // publication and off-lock retirement. EditReply carries its own admission.
    let reply = match crate::server::edit_code_shared(shared, job, source, &commands, started) {
        Ok(reply) => reply,
        Err(error) => return Err(worker::error_text(&error)),
    };
    Ok(feedback.finish(shared, reply, &|| {
        if job.cancel.load(Ordering::Relaxed) || started.elapsed() >= Duration::from_secs(30) {
            Err("Code feedback interrupted".into())
        } else {
            Ok(())
        }
    }))
}

#[cfg(test)]
mod tests;
