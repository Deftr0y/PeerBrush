//! Reviewable COW drafts and interrupted-task metadata, outside editable sources/history.
use crate::{
    engine::{self, Document, Engine, Lease, Scope},
    raster::Raster,
    server::Shared,
};
use serde_json::{json, Value};
use std::{collections::HashMap, sync::Arc};
pub const DRAFT_BUDGET: usize = 256 * 1024 * 1024;
const MAX_PENDING: usize = 4;
const MAX_RECORDS: usize = 16;

#[derive(Clone)]
pub(crate) struct Proposal {
    pub id: String,
    pub actor: String,
    pub label: String,
    pub task: Option<String>,
    pub document: String,
    pub revision: u64,
    pub commands: Vec<Value>,
    pub scopes: Vec<Scope>,
    pub draft: Option<Document>,
    pub expires: u64,
    pub status: String,
    pub reason: String,
    pub created: Value,
    pub operations: Vec<Value>,
}
impl Proposal {
    pub fn state(&self) -> Value {
        json!({"id":self.id,"actor":self.actor,"label":self.label,"task":self.task,"document_id":self.document,"source_revision":self.revision,"scopes":self.scopes,"status":self.status,"reason":self.reason,"expires":self.expires,"created":self.created,"operations":self.operations})
    }
    fn stop(&mut self, status: &str, reason: &str) {
        self.draft = None;
        self.commands.clear();
        self.status = status.into();
        self.reason = reason.into();
    }
}
#[derive(Clone)]
pub(crate) struct TaskRecord {
    pub id: String,
    pub actor: String,
    pub description: String,
    pub scopes: Vec<Scope>,
    pub scope_count: usize,
    pub status: String,
    pub at: u64,
}
impl TaskRecord {
    fn from_lease(l: Lease, status: &str) -> Self {
        Self {
            id: l.id,
            actor: l.owner.chars().take(128).collect(),
            description: l.description.chars().take(240).collect(),
            scope_count: l.scopes.len(),
            scopes: l.scopes.into_iter().take(256).collect(),
            status: status.into(),
            at: engine::now(),
        }
    }
    fn state(&self) -> Value {
        json!({"task":self.id,"actor":self.actor,"description":self.description,"scopes":self.scopes,"scope_count":self.scope_count,"status":self.status,"at":self.at})
    }
}
fn raster_tiles(r: &Raster, out: &mut HashMap<usize, usize>) {
    for tile in r.tiles.values() {
        out.insert(Arc::as_ptr(tile) as usize, tile.len());
    }
    for tile in r.samples16.values() {
        out.insert(Arc::as_ptr(tile) as usize, tile.len() * 2);
    }
    if let Some(original) = &r.retained {
        raster_tiles(&original.pixels, out);
    }
}
fn doc_tiles(doc: &Document, out: &mut HashMap<usize, usize>) {
    for l in &doc.layers {
        raster_tiles(&l.pixels, out);
        if let Some(mask) = &l.mask {
            for s in &mask.steps {
                raster_tiles(&s.pixels, out);
            }
        }
    }
}
impl Engine {
    pub(crate) fn finish_task(&mut self, id: &str, status: &str) {
        if let Some(pos) = self.leases.iter().position(|l| l.id == id) {
            let lease = self.leases.remove(pos);
            self.recent_tasks
                .push(TaskRecord::from_lease(lease, status));
            if self.recent_tasks.len() > 100 {
                self.recent_tasks.remove(0);
            }
            self.activity = self
                .leases
                .last()
                .map(|l| l.description.clone())
                .unwrap_or_default();
            if self.status.starts_with("Reserved by") {
                self.status = "Reservation released · ready to edit".into();
            }
        }
    }
    pub fn take_over_tasks(&mut self) {
        let tasks = self.leases.iter().map(|l| l.id.clone()).collect::<Vec<_>>();
        for task in tasks {
            self.finish_task(&task, "taken_over");
        }
        self.expire_proposals();
        self.status = "You have control · committed AI edits preserved".into();
    }
    pub fn task_recovery(&self) -> Value {
        json!({"active":self.leases,"recent":self.recent_tasks.iter().map(TaskRecord::state).collect::<Vec<_>>()})
    }
    pub(crate) fn expire_proposals(&mut self) {
        for p in &mut self.proposals {
            if p.draft.is_none() {
                continue;
            }
            if p.document != self.doc.id || p.revision != self.doc.revision {
                p.stop(
                    "stale",
                    "The project changed. Request a fresh proposal; your edits are preserved.",
                );
            } else if p.expires <= engine::now() {
                p.stop(
                    "expired",
                    "The proposal expired. Request a fresh proposal from the current project.",
                );
            } else if p.task.as_ref().is_some_and(|task| {
                !self
                    .leases
                    .iter()
                    .any(|l| l.id == *task && l.owner == p.actor)
            }) {
                p.stop(
                    "interrupted",
                    "The task ended, expired or was taken over. No proposal edits were committed.",
                );
            }
        }
    }
    pub fn proposals_state(&mut self) -> Value {
        self.expire();
        json!(self
            .proposals
            .iter()
            .map(Proposal::state)
            .collect::<Vec<_>>())
    }
    pub fn proposal_document(&mut self, id: &str) -> Result<Document, String> {
        self.expire();
        let p = self
            .proposals
            .iter()
            .find(|p| p.id == id)
            .ok_or("Proposal not found in this project")?;
        p.draft.clone().ok_or_else(|| p.reason.clone())
    }
    pub fn reject_proposal(&mut self, actor: &str, id: &str) -> Result<Value, String> {
        self.expire();
        let p = self
            .proposals
            .iter_mut()
            .find(|p| p.id == id)
            .ok_or("Proposal not found in this project")?;
        if actor != "human" && actor != p.actor {
            return Err("Only the human or proposing actor can reject this proposal".into());
        }
        if p.status == "accepted" {
            return Err("This proposal was accepted; use history to undo its edits".into());
        }
        p.stop("rejected", "Proposal discarded; current work preserved.");
        Ok(p.state())
    }
    pub fn accept_proposal(
        &mut self,
        actor: &str,
        id: &str,
        document: &str,
        revision: u64,
    ) -> Result<Value, String> {
        if actor != "human" {
            return Err("Proposals need explicit human acceptance".into());
        }
        self.expire();
        if self.doc.id != document || self.doc.revision != revision {
            return Err("The project changed; review a fresh proposal before accepting".into());
        }
        let p = self
            .proposals
            .iter()
            .find(|p| p.id == id)
            .ok_or("Proposal not found in this project")?
            .clone();
        if p.draft.is_none() {
            return Err(p.reason);
        }
        let mut result = self.commit_proposal(&p)?;
        self.proposals
            .iter_mut()
            .find(|q| q.id == id)
            .unwrap()
            .stop("accepted", "Accepted by the human as one undoable edit.");
        result["proposal"] = json!(id);
        result["accepted_by"] = json!("human");
        self.expire_proposals();
        Ok(result)
    }
    fn queue_proposal(&mut self, p: Proposal) -> Result<Value, String> {
        self.expire();
        if p.document != self.doc.id || p.revision != self.doc.revision {
            return Err(
                "The project changed while preparing the proposal; current work preserved".into(),
            );
        }
        self.check(&p.actor, &p.scopes)?;
        if p.task
            .as_ref()
            .is_some_and(|t| !self.leases.iter().any(|l| l.id == *t && l.owner == p.actor))
        {
            return Err("The task expired or was released while preparing the proposal".into());
        }
        if self.proposals.iter().filter(|p| p.draft.is_some()).count() >= MAX_PENDING {
            return Err("Four proposals are already pending. Review or reject one first".into());
        }
        let mut base = HashMap::new();
        doc_tiles(&self.doc, &mut base);
        let mut pending = HashMap::new();
        for old in self.proposals.iter().chain(std::iter::once(&p)) {
            if let Some(d) = &old.draft {
                doc_tiles(d, &mut pending);
            }
        }
        let bytes: usize = pending
            .iter()
            .filter(|(key, _)| !base.contains_key(*key))
            .map(|(_, n)| n)
            .sum();
        if bytes > DRAFT_BUDGET {
            return Err("Pending proposals exceed the 256 MiB additional native-tile budget. Review or reject a proposal first".into());
        }
        if self.proposals.len() >= MAX_RECORDS {
            let pos = self
                .proposals
                .iter()
                .position(|p| p.draft.is_none())
                .unwrap();
            self.proposals.remove(pos);
        }
        let state = p.state();
        self.proposals.push(p);
        self.status = "AI proposal ready · review before accepting".into();
        Ok(state)
    }
}
/// Prepare outside the shared lock, then publish only against the exact source revision.
pub fn propose(shared: &Shared, actor: &str, p: &Value) -> Result<Value, String> {
    if actor.is_empty() || actor.len() > 128 {
        return Err("Choose an actor name of 1–128 bytes".into());
    }
    let commands = p["commands"]
        .as_array()
        .ok_or("commands must be an array")?;
    if commands.is_empty()
        || commands.len() > 100
        || serde_json::to_vec(commands)
            .map_err(|e| e.to_string())?
            .len()
            > 8 * 1024 * 1024
    {
        return Err("A proposal needs 1–100 commands within 8 MiB".into());
    }
    let document = p["document_id"]
        .as_str()
        .ok_or("Proposals require the observed document_id")?;
    let revision = p["expected_revision"]
        .as_u64()
        .ok_or("Proposals require the observed expected_revision")?;
    let label = p["label"].as_str().unwrap_or("Proposed AI edit");
    if label.is_empty() || label.chars().count() > 160 {
        return Err("Choose a proposal label of 1–160 characters".into());
    }
    let task = p["task"].as_str();
    let mut draft = Engine::new();
    {
        let mut e = shared.lock().unwrap();
        e.expire();
        if e.doc.id != document || e.doc.revision != revision {
            return Err("The proposal source changed. Observe the current project again".into());
        }
        draft.doc = e.doc.clone();
        draft.leases = e.leases.clone();
        draft.brush_library = e.brush_library.clone();
        draft.filter_library = e.filter_library.clone();
    }
    let commands = draft
        .filter_library
        .lock()
        .unwrap()
        .resolve_commands(commands)?;
    let commands = draft
        .brush_library
        .lock()
        .unwrap()
        .resolve_commands(&commands)?;
    let result = draft.edit(actor, &commands, Some(revision), task, label)?;
    if draft.doc.id != document {
        return Err("Proposals edit the current project; new/open are document actions".into());
    }
    let scopes = draft
        .undo
        .last()
        .ok_or("Missing proposal scope")?
        .scopes
        .clone();
    let proposal = Proposal {
        id: engine::id(),
        actor: actor.into(),
        label: label.into(),
        task: task.map(String::from),
        document: document.into(),
        revision,
        commands: commands.clone(),
        scopes,
        draft: Some(draft.doc),
        expires: engine::now() + 900,
        status: "pending".into(),
        reason: String::new(),
        created: result["created"].clone(),
        operations: commands.iter().map(|c| c["op"].clone()).collect(),
    };
    shared.lock().unwrap().queue_proposal(proposal)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::{Mask, MaskStep};
    use std::sync::Mutex;
    #[test]
    fn accounting_deduplicates_shared_tiles_but_includes_native_masks_and_originals() {
        let mut doc = Document::new_depth(16, 16, 16).unwrap();
        doc.layers[0]
            .pixels
            .set16(1, 1, [12345, 23457, 34569, 65535]);
        let mut base = HashMap::new();
        doc_tiles(&doc, &mut base);
        assert_eq!(base.values().sum::<usize>(), 256 * 256 * 4 * 2);
        let mut draft = doc.clone();
        draft.layers[0]
            .pixels
            .set16(1, 1, [23457, 34569, 45671, 65535]);
        draft.layers[0].pixels.retained = Some(Box::new(crate::retained::Original::from(
            &doc.layers[0].pixels,
        )));
        let mut mask = Raster::new_depth(16, 16, 16);
        mask.set16(2, 2, [12345, 12345, 12345, 65535]);
        draft.layers[0].mask = Some(Mask {
            enabled: true,
            cache_key: engine::id(),
            steps: vec![MaskStep {
                weight: 1.0,
                id: engine::id(),
                kind: "paint".into(),
                enabled: true,
                value: 255.,
                pixels: mask,
                settings: json!({}),
            }],
        });
        let mut pending = HashMap::new();
        doc_tiles(&draft, &mut pending);
        doc_tiles(&draft.clone(), &mut pending);
        assert_eq!(pending.len(), 3);
        assert_eq!(
            pending
                .iter()
                .filter(|(key, _)| !base.contains_key(*key))
                .map(|(_, n)| n)
                .sum::<usize>(),
            2 * 256 * 256 * 4 * 2
        );
    }
    #[test]
    fn proposal_expiry_drops_sources_without_history_or_document_changes() {
        let shared = Arc::new(Mutex::new(Engine::new()));
        let doc = shared.lock().unwrap().doc.clone();
        let p = propose(&shared,"draft-agent",&json!({"document_id":doc.id,"expected_revision":0,"commands":[{"op":"layer.update","layer":doc.layers[0].id,"name":"Draft"}]})).unwrap();
        let mut e = shared.lock().unwrap();
        e.proposals[0].expires = 0;
        e.expire();
        assert_eq!(e.proposals_state()[0]["status"], "expired");
        assert!(e.proposal_document(p["id"].as_str().unwrap()).is_err());
        assert!(e.undo.is_empty());
        assert_eq!(e.doc.revision, 0);
        assert_eq!(e.doc.layers[0].name, doc.layers[0].name);
        assert!(e.proposals[0].commands.is_empty());
    }
}
