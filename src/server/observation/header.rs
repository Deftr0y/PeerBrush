//! Only the bounded observation projection is copied while Engine is held.
//! Commands, proposal drafts, document pixels and progress previews are absent
//! from this header because the legacy observation does not serialize them.
use crate::{
    collaboration::{Proposal, TaskRecord},
    engine::{AiChange, Engine, Lease, Scope},
};
use serde::{ser::SerializeSeq, Serialize, Serializer};
use serde_json::Value;

#[derive(Serialize)]
pub(super) struct Header<'a> {
    pub file_status: &'a str,
    pub reservations: &'a [Lease],
    pub ai_change: &'a Option<AiChange>,
    pub dirty: bool,
    pub project_id: &'a str,
    pub closed: bool,
    proposals: Proposals<'a>,
    task_recovery: TaskRecovery<'a>,
}
impl<'a> Header<'a> {
    pub(super) fn new(engine: &'a Engine, revision: u64) -> Self {
        Self {
            file_status: &engine.status,
            reservations: &engine.leases,
            ai_change: &engine.ai_change,
            dirty: revision != engine.saved_revision,
            project_id: &engine.project_id,
            closed: engine.closed,
            proposals: Proposals(&engine.proposals),
            task_recovery: TaskRecovery {
                active: &engine.leases,
                recent: Tasks(&engine.recent_tasks),
            },
        }
    }
}
#[derive(Serialize)]
struct ProposalWire<'a> {
    id: &'a str,
    actor: &'a str,
    label: &'a str,
    task: Option<&'a str>,
    document_id: &'a str,
    source_revision: u64,
    scopes: &'a [Scope],
    status: &'a str,
    reason: &'a str,
    expires: u64,
    created: &'a Value,
    operations: &'a [Value],
}
struct Proposals<'a>(&'a [Proposal]);
impl Serialize for Proposals<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut seq = serializer.serialize_seq(Some(self.0.len()))?;
        for p in self.0 {
            seq.serialize_element(&ProposalWire {
                id: &p.id,
                actor: &p.actor,
                label: &p.label,
                task: p.task.as_deref(),
                document_id: &p.document,
                source_revision: p.revision,
                scopes: &p.scopes,
                status: &p.status,
                reason: &p.reason,
                expires: p.expires,
                created: &p.created,
                operations: &p.operations,
            })?;
        }
        seq.end()
    }
}
#[derive(Serialize)]
struct TaskRecovery<'a> {
    active: &'a [Lease],
    recent: Tasks<'a>,
}
#[derive(Serialize)]
struct TaskWire<'a> {
    task: &'a str,
    actor: &'a str,
    description: &'a str,
    scopes: &'a [Scope],
    scope_count: usize,
    status: &'a str,
    at: u64,
}
struct Tasks<'a>(&'a [TaskRecord]);
impl Serialize for Tasks<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut seq = serializer.serialize_seq(Some(self.0.len()))?;
        for t in self.0 {
            seq.serialize_element(&TaskWire {
                task: &t.id,
                actor: &t.actor,
                description: &t.description,
                scopes: &t.scopes,
                scope_count: t.scope_count,
                status: &t.status,
                at: t.at,
            })?;
        }
        seq.end()
    }
}
