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
pub(crate) mod native;

#[derive(serde::Serialize, serde::Deserialize)]
pub(crate) struct Proposal {
    pub id: String,
    pub actor: String,
    pub label: String,
    pub task: Option<String>,
    #[serde(rename = "document_id")]
    pub document: String,
    #[serde(rename = "source_revision")]
    pub revision: u64,
    pub commands: Vec<Value>,
    pub scopes: Vec<Scope>,
    #[serde(skip)]
    pub draft: Option<Arc<Document>>,
    #[serde(skip)]
    pub native: Option<native::Binding>,
    #[serde(skip)]
    pub native_charge: Option<Arc<crate::psd::memory36::MetadataCharge>>,
    pub expires: u64,
    pub status: String,
    pub reason: String,
    pub created: Value,
    pub operations: Vec<Value>,
}
impl Proposal {
    fn pending(&self) -> bool {
        self.draft.is_some() || self.native.is_some()
    }
    pub fn state(&self) -> Value {
        serde_json::to_value(self.wire()).expect("Proposal metadata is serializable")
    }
    pub(crate) fn wire(&self) -> impl serde::Serialize + '_ {
        #[derive(serde::Serialize)]
        struct Wire<'a> {
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
        Wire {
            id: &self.id,
            actor: &self.actor,
            label: &self.label,
            task: self.task.as_deref(),
            document_id: &self.document,
            source_revision: self.revision,
            scopes: &self.scopes,
            status: &self.status,
            reason: &self.reason,
            expires: self.expires,
            created: &self.created,
            operations: &self.operations,
        }
    }
    fn stop(&mut self, status: &str, reason: &str) {
        self.draft = None;
        self.native = None;
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
        if !original.warps.is_empty() {
            out.insert(
                original.warps.as_ptr() as usize,
                original.warps.capacity() * std::mem::size_of::<crate::warp::Stage>(),
            );
        }
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
        let document = self.authority.view();
        for p in &mut self.proposals {
            if !p.pending() {
                continue;
            }
            if p.document != document.id()
                || p.revision != document.revision()
                || p.native
                    .as_ref()
                    .is_some_and(|binding| !binding.matches(document, &self.project_id))
            {
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
        self.proposal_snapshot(id)
            .map(|document| (*document).clone())
    }
    pub(crate) fn proposal_snapshot(&mut self, id: &str) -> Result<Arc<Document>, String> {
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
        let view = engine::view::current(self);
        if view.id() != document || view.revision() != revision {
            return Err("The project changed; review a fresh proposal before accepting".into());
        }
        let pos = self
            .proposals
            .iter()
            .position(|p| p.id == id)
            .ok_or("Proposal not found in this project")?;
        if self.proposals[pos].draft.is_none() {
            return Err(self.proposals[pos].reason.clone());
        }
        // The transaction can expire other records. Keep this sole proposal
        // owner outside the list and restore it on both success and refusal.
        let proposal = self.proposals.remove(pos);
        let result = self.commit_proposal(&proposal);
        self.proposals.insert(pos, proposal);
        let mut result = match result {
            Ok(result) => result,
            Err(reason) => {
                self.expire_proposals();
                return Err(reason);
            }
        };
        self.proposals[pos].stop("accepted", "Accepted by the human as one undoable edit.");
        result["proposal"] = json!(id);
        result["accepted_by"] = json!("human");
        self.expire_proposals();
        Ok(result)
    }
    fn queue_proposal(&mut self, p: Proposal) -> Result<Value, String> {
        self.expire();
        let view = engine::view::current(self);
        if p.document != view.id() || p.revision != view.revision() {
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
        doc_tiles(self.resident()?, &mut base);
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
    validate_commands(commands, &|| Ok(()))?;
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
    let mut draft = {
        let mut e = shared.lock().unwrap();
        e.expire();
        let view = engine::view::current(&e);
        if view.id() != document || view.revision() != revision {
            return Err("The proposal source changed. Observe the current project again".into());
        }
        e.resident_draft()?
    };
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
    if engine::view::current(&draft).id() != document {
        return Err("Proposals edit the current project; new/open are document actions".into());
    }
    let scopes = draft
        .history_entries()
        .next_back()
        .ok_or("Missing proposal scope")?
        .scopes()
        .to_vec();
    let operations = commands.iter().map(|c| c["op"].clone()).collect();
    let proposal = Proposal {
        id: engine::id(),
        actor: actor.into(),
        label: label.into(),
        task: task.map(String::from),
        document: document.into(),
        revision,
        commands,
        scopes,
        draft: Some(Arc::new(draft.into_resident_document()?)),
        native: None,
        native_charge: None,
        expires: engine::now() + 900,
        status: "pending".into(),
        reason: String::new(),
        created: result["created"].clone(),
        operations,
    };
    shared.lock().unwrap().queue_proposal(proposal)
}

fn validate_commands(
    commands: &[Value],
    check: &dyn Fn() -> Result<(), String>,
) -> Result<(), String> {
    if commands.is_empty() || commands.len() > 100 {
        return Err("A proposal needs 1–100 commands within 8 MiB".into());
    }
    crate::psd::canonical36::Canonical::raw_length(
        &commands,
        crate::psd::canonical36::Limits {
            bytes: 8 * 1024 * 1024,
            nodes: 8_388_608,
            depth: 128,
        },
        check,
    )
    .map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::{Mask, MaskStep};
    use std::sync::Mutex;
    #[test]
    fn proposal_wire_bound_counts_escaped_bytes_without_materializing_another_payload() {
        assert!(validate_commands(&[], &|| Ok(())).is_err());
        assert!(validate_commands(&vec![json!({}); 101], &|| Ok(())).is_err());
        let mut command = Value::String("a".repeat(8 * 1024 * 1024 - 4));
        assert!(validate_commands(std::slice::from_ref(&command), &|| Ok(())).is_ok());
        if let Value::String(text) = &mut command {
            text.push('a');
        }
        assert!(validate_commands(std::slice::from_ref(&command), &|| Ok(())).is_err());
        assert!(
            validate_commands(&[Value::String("\n".repeat(4 * 1024 * 1024))], &|| Ok(())).is_err()
        );
        assert_eq!(
            validate_commands(&[json!({})], &|| Err("Stop validation".into())).unwrap_err(),
            "Stop validation"
        );
    }
    #[test]
    fn frozen_review_shares_one_document_and_refused_acceptance_keeps_the_proposal_owner() {
        for depth in [8, 16] {
            let mut document = Document::new_depth(16, 12, depth).unwrap();
            document.layers[0]
                .pixels
                .set16(1, 1, [12347, 23459, 34571, 61379]);
            let layer = document.layers[0].id.clone();
            let shared = Arc::new(Mutex::new(Engine::from_document(document.clone())));
            let mut ids = Vec::new();
            for label in ["First", "Second"] {
                let state = propose(
                    &shared,
                    "agent",
                    &json!({
                        "document_id": document.id, "expected_revision": document.revision,
                        "commands":[{"op":"layer.update","layer":layer,"name":label}],
                    }),
                )
                .unwrap();
                ids.push(state["id"].as_str().unwrap().to_owned());
            }
            let mut engine = shared.lock().unwrap();
            let snapshot = engine.proposal_snapshot(&ids[0]).unwrap();
            assert!(Arc::ptr_eq(
                &snapshot,
                &engine.proposal_snapshot(&ids[0]).unwrap()
            ));
            let frozen = serde_json::to_value(&*snapshot).unwrap();
            let commands = engine.proposals[0].commands.as_ptr();
            let task = engine
                .reserve(
                    "other-agent",
                    "Protect during review",
                    vec![Scope::layer(&layer)],
                )
                .unwrap();
            assert!(engine
                .accept_proposal("human", &ids[0], &document.id, document.revision)
                .unwrap_err()
                .contains("Reserved"));
            assert_eq!(
                engine.proposals.iter().map(|p| &p.id).collect::<Vec<_>>(),
                ids.iter().collect::<Vec<_>>()
            );
            assert_eq!(engine.proposals[0].commands.as_ptr(), commands);
            assert!(Arc::ptr_eq(
                &snapshot,
                &engine.proposal_snapshot(&ids[0]).unwrap()
            ));
            assert_eq!(
                serde_json::to_value(&engine.resident_state().expect("resident fixture").document)
                    .unwrap(),
                serde_json::to_value(&document).unwrap()
            );
            assert!(engine
                .resident_state()
                .expect("resident fixture")
                .undo
                .is_empty());
            engine.finish_task(&task.id, "ended");
            engine
                .resident_state_mut()
                .expect("resident fixture")
                .document
                .name = "Human Save As.psd".into();
            engine
                .accept_proposal("human", &ids[0], &document.id, document.revision)
                .unwrap();
            assert_eq!(engine.proposals[0].status, "accepted");
            assert_eq!(engine.proposals[1].status, "stale");
            assert_eq!(
                engine
                    .resident_state()
                    .expect("resident fixture")
                    .document
                    .name,
                "Human Save As.psd"
            );
            assert_eq!(
                engine
                    .resident_state()
                    .expect("resident fixture")
                    .undo
                    .len(),
                1
            );
            assert_eq!(
                engine.resident_state().expect("resident fixture").undo[0].actor,
                "agent"
            );
            assert_eq!(serde_json::to_value(&*snapshot).unwrap(), frozen);
            assert_eq!(
                snapshot.layers[0].pixels.get16(1, 1),
                document.layers[0].pixels.get16(1, 1)
            );
            engine.undo("human").unwrap();
            assert_eq!(
                engine
                    .resident_state()
                    .expect("resident fixture")
                    .document
                    .layers[0]
                    .pixels
                    .get16(1, 1),
                document.layers[0].pixels.get16(1, 1)
            );
        }
    }
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
            linked: true,
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
    fn warp_metadata_is_counted_for_each_owned_chain_while_original_tiles_stay_shared() {
        for depth in [8, 16] {
            let mut doc = Document::new_depth(16, 16, depth).unwrap();
            doc.layers[0]
                .pixels
                .set16(1, 1, [12347, 23459, 34571, 65535]);
            let mut original = crate::retained::Original::from(&doc.layers[0].pixels);
            original.warps.push(crate::warp::Stage {
                before: crate::projective::Map::IDENTITY.matrix(),
                grid: crate::warp::Grid::identity([-2., -2., 18., 18.]).unwrap(),
            });
            original.validate(depth).unwrap();
            let metadata = original.warps.capacity() * std::mem::size_of::<crate::warp::Stage>();
            let tiles = original.pixels.bytes();
            doc.layers[0].pixels.retained = Some(Box::new(original));
            assert_eq!(doc.layers[0].pixels.stored_bytes(), 2 * tiles + metadata);
            let clone = doc.clone();
            let cloned_metadata = clone.layers[0]
                .pixels
                .retained
                .as_ref()
                .unwrap()
                .warps
                .capacity()
                * std::mem::size_of::<crate::warp::Stage>();
            let mut shared = HashMap::new();
            doc_tiles(&doc, &mut shared);
            doc_tiles(&clone, &mut shared);
            assert_eq!(
                shared.values().sum::<usize>(),
                tiles + metadata + cloned_metadata
            );
        }
    }
    #[test]
    fn proposal_expiry_drops_sources_without_history_or_document_changes() {
        let shared = Arc::new(Mutex::new(Engine::new()));
        let doc = shared
            .lock()
            .unwrap()
            .resident_state()
            .expect("resident fixture")
            .document
            .clone();
        let p = propose(&shared,"draft-agent",&json!({"document_id":doc.id,"expected_revision":0,"commands":[{"op":"layer.update","layer":doc.layers[0].id,"name":"Draft"}]})).unwrap();
        let mut e = shared.lock().unwrap();
        e.proposals[0].expires = 0;
        e.expire();
        assert_eq!(e.proposals_state()[0]["status"], "expired");
        assert!(e.proposal_document(p["id"].as_str().unwrap()).is_err());
        assert!(e
            .resident_state()
            .expect("resident fixture")
            .undo
            .is_empty());
        assert_eq!(
            e.resident_state()
                .expect("resident fixture")
                .document
                .revision,
            0
        );
        assert_eq!(
            e.resident_state()
                .expect("resident fixture")
                .document
                .layers[0]
                .name,
            doc.layers[0].name
        );
        assert!(e.proposals[0].commands.is_empty());
    }
}
