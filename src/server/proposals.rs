//! Native proposal transport. Source work uses the shared stored candidates;
//! metadata and PNG owners stay admitted through the actual response handoff.
use super::{observation::pixels::Pixels, reply::ReplyAdmission, EditControl, Shared};
use crate::{
    collaboration::native,
    engine::{self, report::header},
    large_document::ResidentBudget,
};
use serde::Serialize;
use serde_json::{json, Value};
#[cfg(test)]
mod tests;
type Result<T> = std::result::Result<T, String>;

fn project<T: Serialize + ?Sized>(
    admission: &mut ReplyAdmission,
    pool: &ResidentBudget,
    value: &T,
) -> Result<Value> {
    admission.admit_projection(pool, value)?;
    serde_json::to_value(value).map_err(|e| e.to_string())
}
fn state(
    shared: &Shared,
    id: &str,
    pool: &ResidentBudget,
    admission: &mut ReplyAdmission,
) -> Result<Value> {
    let mut engine = shared.lock().unwrap();
    engine.expire();
    let proposal = engine
        .proposals
        .iter()
        .find(|p| p.id == id)
        .ok_or("Proposal not found in this project")?;
    let result = project(admission, pool, &proposal.wire());
    result
}
pub(super) fn native(
    shared: &Shared,
    actor: &str,
    p: &Value,
    admission: &mut ReplyAdmission,
) -> Result<Option<Value>> {
    let context = {
        let engine = shared.lock().unwrap();
        let Some(document) = engine.native_document() else {
            return Ok(None);
        };
        crate::workspace::validate_target(&engine, p)?;
        (
            document.budget().clone(),
            engine.workspace.as_ref().and_then(std::sync::Weak::upgrade),
            engine.project_id.clone(),
        )
    };
    let (pool, workspace, project_id) = context;
    // Reject a detached Value transport before create, acceptance or rejection
    // can mutate a record. The source pool remains with the final sealed reply.
    admission.admit(&pool, 4096)?;
    let action = p["action"].as_str().ok_or("Missing proposal action")?;
    if action == "list" {
        #[derive(Serialize)]
        struct Listing<'a> {
            proposals: header::Proposals<'a>,
            tasks: header::TaskRecovery<'a>,
        }
        let mut engine = shared.lock().unwrap();
        engine.expire();
        let result = project(
            admission,
            &pool,
            &Listing {
                proposals: header::Proposals(&engine.proposals),
                tasks: header::task_recovery(&engine),
            },
        )?;
        return Ok(Some(result));
    }
    if !["create", "preview", "accept", "reject"].contains(&action) {
        return Err("Unknown proposal action".into());
    }
    let rect = p
        .get("rect")
        .filter(|v| !v.is_null())
        .map(|value| engine::rect(value).ok_or("Invalid proposal preview rectangle"))
        .transpose()?;
    let id = if action == "create" {
        let created = native::create_stored(
            shared,
            actor,
            p["commands"]
                .as_array()
                .ok_or("commands must be an array")?,
            p["document_id"]
                .as_str()
                .ok_or("Proposals require the observed document_id")?,
            p["expected_revision"]
                .as_u64()
                .ok_or("Proposals require the observed expected_revision")?,
            p["label"].as_str().unwrap_or("Proposed AI edit"),
            p["task"].as_str(),
        )?;
        created["id"]
            .as_str()
            .ok_or("Created proposal has no ID")?
            .to_owned()
    } else {
        p["proposal"]
            .as_str()
            .ok_or("Missing proposal ID")?
            .to_owned()
    };
    if action == "reject" {
        // Admit the selected metadata and bounded new terminal strings first.
        let result = {
            let mut engine = shared.lock().unwrap();
            crate::workspace::validate_target(&engine, p)?;
            engine.expire();
            let proposal = engine
                .proposals
                .iter()
                .find(|p| p.id == id)
                .ok_or("Proposal not found in this project")?;
            admission.admit_projection(&pool, &proposal.wire())?;
            engine.reject_proposal(actor, &id)?
        };
        if let Some(workspace) = workspace {
            native::maintain_workspace(&workspace);
        }
        return Ok(Some(result));
    }
    if action == "accept" {
        let reply = native::accept_stored(
            shared,
            actor,
            &id,
            p["document_id"]
                .as_str()
                .ok_or("Acceptance needs the reviewed document_id")?,
            p["expected_revision"]
                .as_u64()
                .ok_or("Acceptance needs the reviewed expected_revision")?,
            &EditControl::default(),
        )?;
        let mut result = reply.into_response(admission)?;
        result["proposal"] = json!(id);
        result["accepted_by"] = json!("human");
        if p["feedback"] != "request" {
            let view = json!({"project_id":project_id,"document_id":p["document_id"],"expected_revision":result["revision"],"rect":rect,"max_edge":p["max_edge"].as_u64().unwrap_or(768).clamp(32,4096)});
            match super::observation(shared, &view, admission) {
                Ok(mut feedback) => result["images"] = feedback["images"].take(),
                Err(_) => {
                    result["images"] = json!([]);
                    result["feedback_warning"] = json!("Edit applied; canvas feedback is unavailable. Observe the current project.");
                }
            }
        }
        return Ok(Some(result));
    }
    let guard = native::review_guard(shared, &id)?;
    let proposal = state(shared, &id, &pool, admission)?;
    let mut result = json!({"proposal":proposal,"images":[]});
    if action == "preview" || p["feedback"] != "request" {
        let rendered = native::review_stored(
            shared,
            &id,
            rect,
            p["max_edge"].as_u64().unwrap_or(768).clamp(32, 4096) as u32,
            None,
            false,
            None,
            &|| guard.check(shared),
        )?;
        let (origin, frame, supplement) = rendered.into_parts();
        drop((origin, supplement));
        let (png, tag) = Pixels::new(frame).encode(&pool, &mut || guard.check(shared))?;
        let key = tag.key();
        let image = json!({"mime_type":"image/png","data":null,"width":key.width,"height":key.height,"document_rect":key.rect,"revision":result["proposal"]["source_revision"],"proposal":id,"preview_only":true});
        admission.admit_projection(&pool, &image)?;
        admission.image(0, png)?;
        result["images"] = json!([image]);
    }
    guard.check(shared)?;
    Ok(Some(result))
}
