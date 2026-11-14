//! Exclusive document and history ownership. Callers inspect borrowed views;
//! native preparation and post-publication retirement happen outside Engine locks.
use super::{native, view, Document, Engine, History, Lease};
use std::{
    collections::BTreeSet,
    sync::{Arc, OnceLock},
};

type Result<T> = std::result::Result<T, String>;

/// Immutable preparation identity, captured from live Engine authority only.
/// It is neither an editable document nor filesystem publication permission.
pub(crate) struct NativeSeed {
    document: native::NativeDocument,
    project_id: String,
    ai_serial: u64,
    path: Option<std::path::PathBuf>,
    file_version: Option<crate::file_safety::Version>,
    ownership: Option<Arc<crate::file_ownership::Ownership>>,
    _charge: Arc<crate::psd::memory36::MetadataCharge>,
}
impl NativeSeed {
    pub(crate) fn document(&self) -> &native::NativeDocument {
        &self.document
    }
    pub(super) fn runtime_serial(&self) -> u64 {
        self.ai_serial
    }
    pub(crate) fn project_id(&self) -> &str {
        &self.project_id
    }
    pub(crate) fn source_path(&self) -> Option<&std::path::Path> {
        self.path.as_deref()
    }
    pub(crate) fn file_version(&self) -> Option<crate::file_safety::Version> {
        self.file_version
    }
    pub(super) fn capture(engine: &Engine, document: &native::NativeDocument) -> Result<Self> {
        let bytes = engine
            .project_id
            .len()
            .checked_add(engine.path.as_ref().map_or(0, |p| p.as_os_str().len()))
            .and_then(|n| n.checked_add(2048))
            .ok_or("Native dispatch identity admission overflow")?;
        let charge =
            crate::psd::memory36::MetadataCharge::reserve(document.budget(), bytes, &|| Ok(()))?;
        let mut project_id = String::new();
        project_id
            .try_reserve_exact(engine.project_id.len())
            .map_err(|_| "Native project identity allocation refused")?;
        project_id.push_str(&engine.project_id);
        let path = engine
            .path
            .as_ref()
            .map(|path| -> Result<_> {
                let mut copy = std::path::PathBuf::new();
                copy.try_reserve_exact(path.as_os_str().len())
                    .map_err(|_| "Native path identity allocation refused")?;
                copy.push(path);
                Ok(copy)
            })
            .transpose()?;
        Ok(Self {
            document: document.clone(),
            project_id,
            ai_serial: engine.ai_serial,
            path,
            file_version: engine.file_version,
            ownership: engine.file_ownership.clone(),
            _charge: charge,
        })
    }
    pub(crate) fn validate(&self, engine: &Engine) -> Result<()> {
        let document = engine
            .native_document()
            .ok_or("Native dispatch authority changed")?;
        let ownership = match (&self.ownership, &engine.file_ownership) {
            (None, None) => true,
            (Some(a), Some(b)) => Arc::ptr_eq(a, b),
            _ => false,
        };
        if document.key() != self.document.key()
            || document.metadata().id() != self.document.metadata().id()
            || document.metadata().revision() != self.document.metadata().revision()
            || self.project_id != engine.project_id
            || self.path != engine.path
            || self.file_version != engine.file_version
            || !ownership
            || !document.budget().same_pool(self.document.budget())
            || !document
                .disk_budget()
                .same_pool(self.document.disk_budget())
        {
            return Err("Native dispatch source observation is stale".into());
        }
        Ok(())
    }
}
impl Engine {
    fn native_authority(&self) -> Option<&DocumentAuthority> {
        self.authority.native().ok().map(|_| &self.authority)
    }
    pub(crate) fn history_task_complete(&self, owner: &str, task: &str) -> bool {
        self.authority.task_complete(owner, task)
    }
    pub(crate) fn native_document(&self) -> Option<&native::NativeDocument> {
        self.native_authority()
            .and_then(|state| state.native().ok())
    }
    pub(crate) fn native_seed(&self) -> Result<Option<NativeSeed>> {
        self.ensure_open()?;
        self.native_document()
            .map(|document| NativeSeed::capture(self, document))
            .transpose()
    }
    /// Exchange only document-scoped state. The staging engine receives the old
    /// owners and must outlive the publication locks.
    pub(crate) fn replace_prepared(
        &mut self,
        prepared: &mut Self,
        actor: &str,
        discard: bool,
        loading: Option<&crate::loading::Control>,
    ) -> Result<()> {
        self.ensure_write_available()?;
        if prepared.project_id != self.project_id || prepared.closed {
            return Err("Prepared replacement belongs to another project".into());
        }
        if self.authority.view().revision() != self.saved_revision && !discard {
            return Err("Unsaved work: save first or explicitly pass discard=true".into());
        }
        let owns_loading = match (&self.loading, loading) {
            (None, None) => true,
            (Some(active), Some(request)) => active.same(request),
            _ => false,
        };
        let saving = self
            .saving
            .as_ref()
            .is_some_and(|save| save.read_status(|status| !status.finished));
        if !owns_loading || saving || self.has_running_code() {
            return Err(
                "Finish the active open, save or code job before replacing the canvas".into(),
            );
        }
        if let Some(request) = loading {
            request.check()?;
        }
        if let Some(lease) = self.leases.iter().find(|lease| {
            lease.owner != actor && lease.expires > super::now() && !lease.scopes.is_empty()
        }) {
            return Err(format!(
                "Reserved by {}: {}. Release the reservation before replacing the canvas.",
                lease.owner, lease.description
            ));
        }
        use std::mem::swap;
        swap(&mut self.authority, &mut prepared.authority);
        swap(&mut self.saved_revision, &mut prepared.saved_revision);
        swap(&mut self.path, &mut prepared.path);
        swap(&mut self.file_version, &mut prepared.file_version);
        swap(&mut self.file_ownership, &mut prepared.file_ownership);
        swap(&mut self.file_state_charge, &mut prepared.file_state_charge);
        swap(&mut self.status, &mut prepared.status);
        swap(&mut self.leases, &mut prepared.leases);
        swap(&mut self.changes, &mut prepared.changes);
        swap(&mut self.ai_change, &mut prepared.ai_change);
        swap(&mut self.proposals, &mut prepared.proposals);
        swap(&mut self.recent_tasks, &mut prepared.recent_tasks);
        swap(&mut self.code_jobs, &mut prepared.code_jobs);
        self.activity.clear();
        self.native_close_review = None;
        Ok(())
    }
    pub(crate) fn capture_native_history(
        &self,
        seed: &NativeSeed,
    ) -> Result<native::HistorySnapshot> {
        self.ensure_write_available()?;
        seed.validate(self)?;
        self.native_authority()
            .ok_or("Native history authority is unavailable")?
            .native_history()
    }
    pub(crate) fn validate_native_save_name(
        &self,
        seed: &NativeSeed,
        prepared: &native::PreparedSaveName,
    ) -> Result<()> {
        self.ensure_open()?;
        seed.validate(self)?;
        self.authority.validate_save_name(prepared)
    }
    pub(crate) fn commit_native_save_name(
        &mut self,
        seed: &NativeSeed,
        control: &crate::saving::Control,
        prepared: native::PreparedSaveName,
    ) -> std::result::Result<native::NativeDocument, native::RefusedSaveName> {
        if let Err(reason) = self.validate_native_save_name(seed, &prepared) {
            return Err(prepared.refused(reason));
        }
        if !self.saving.as_ref().is_some_and(|save| save.same(control))
            || control.read_status(|status| {
                status.can_cancel
                    || status.cancel_requested
                    || status.finished
                    || status.document_id != seed.document().metadata().id()
                    || status.revision != seed.document().metadata().revision()
            })
        {
            return Err(
                prepared.refused("Native saved name requires its committed save request".into())
            );
        }
        self.authority.commit_save_name(prepared)
    }
    pub(crate) fn move_native_history(
        &mut self,
        seed: &NativeSeed,
        actor: &str,
        prepared: native::PreparedMove,
    ) -> std::result::Result<native::NativeDocument, native::RefusedMove> {
        if let Err(reason) = self
            .ensure_write_available()
            .and_then(|_| seed.validate(self))
        {
            return Err(prepared.refused(&reason));
        }
        self.authority.move_native(
            native::Publication {
                actor,
                leases: &self.leases,
                now: super::now(),
                closed: self.closed,
                write_available: true,
            },
            prepared,
        )
    }
    pub(crate) fn capture_native(
        &self,
        seed: &NativeSeed,
        descriptor: Arc<native::CommandDescriptor>,
        actor: &str,
        expected: Option<u64>,
        document_id: Option<&str>,
    ) -> Result<NativeCapture> {
        self.ensure_write_available()?;
        seed.validate(self)?;
        if expected.is_some_and(|revision| revision != seed.document.metadata().revision())
            || document_id.is_some_and(|id| id != seed.document.metadata().id())
        {
            return Err("Native edit differs from its observed document or revision".into());
        }
        self.native_authority()
            .ok_or("Native edit authority is unavailable")?
            .capture_native(
                descriptor,
                actor,
                &self.leases,
                super::now(),
                self.closed,
                true,
            )
    }
    pub(crate) fn commit_native(
        &mut self,
        seed: &NativeSeed,
        actor: &str,
        prepared: native::PreparedCommit,
    ) -> std::result::Result<native::RetiredHistory, native::RefusedCommit> {
        if let Err(reason) = self
            .ensure_write_available()
            .and_then(|_| seed.validate(self))
        {
            return Err(prepared.refused(&reason));
        }
        self.authority.commit_native(
            native::Publication {
                actor,
                leases: &self.leases,
                now: super::now(),
                closed: self.closed,
                write_available: true,
            },
            prepared,
        )
    }

    pub(super) fn check_native_commit(
        &self,
        seed: &NativeSeed,
        actor: &str,
        at: u64,
        prepared: &native::PreparedCommit,
    ) -> Result<()> {
        self.ensure_write_available()?;
        seed.validate(self)?;
        let DocumentAuthority::Native(state) = &self.authority else {
            return Err("Native publication authority changed".into());
        };
        state.history.check_commit(
            &state.document,
            native::Publication {
                actor,
                leases: &self.leases,
                now: at,
                closed: self.closed,
                write_available: true,
            },
            prepared,
        )
    }

    /// The paired commit keeps this Engine exclusively borrowed after checking
    /// the exact prepared edit. No project or lease state can change in between.
    pub(super) fn apply_checked_native_commit(
        &mut self,
        prepared: native::PreparedCommit,
    ) -> native::RetiredHistory {
        let DocumentAuthority::Native(state) = &mut self.authority else {
            unreachable!("checked native authority");
        };
        let damage = prepared.raster_damage();
        let retired = state.history.commit_checked(&mut state.document, prepared);
        state.raster_damage = damage;
        retired
    }
}

pub(crate) enum DocumentAuthority {
    Resident(ResidentAuthority),
    Native(NativeAuthority),
}
pub struct ResidentAuthority {
    pub document: Document,
    pub undo: Vec<History>,
    pub redo: Vec<History>,
    pub truncated_tasks: BTreeSet<(String, String)>,
    snapshot: OnceLock<Arc<Document>>,
}
pub(crate) struct NativeAuthority {
    document: native::NativeDocument,
    history: native::NativeHistory,
    raster_damage: Option<native::worker::RasterDamage>,
}
/// This contains immutable source/metadata owners and a pure history observation.
/// It grants no filesystem permission and retains no UI or Engine mutex.
pub(crate) struct NativeCapture {
    document: native::NativeDocument,
    history: native::HistorySnapshot,
    edit: native::CapturedEdit,
}
#[derive(Clone, Copy)]
pub(crate) enum HistoryEntry<'a> {
    Resident(&'a History),
    Native(native::HistoryEntry<'a>),
}
pub(crate) enum HistoryEntries<'a> {
    Resident(std::slice::Iter<'a, History>),
    Native(native::HistoryEntries<'a>),
}
macro_rules! history_borrowed {
    ($name:ident,$resident:ident,$ty:ty) => {
        pub(crate) fn $name(self) -> $ty {
            match self {
                Self::Resident(entry) => &entry.$resident,
                Self::Native(entry) => entry.$name(),
            }
        }
    };
}
impl<'a> HistoryEntry<'a> {
    history_borrowed!(actor, actor, &'a str);
    history_borrowed!(label, label, &'a str);
    history_borrowed!(scopes, scopes, &'a [super::Scope]);
    history_borrowed!(reverted_batches, reverted, &'a [u64]);
    pub(crate) fn task(self) -> Option<&'a str> {
        match self {
            Self::Resident(entry) => entry.task.as_deref(),
            Self::Native(entry) => entry.task(),
        }
    }
    pub(crate) fn gesture(self) -> Option<&'a str> {
        match self {
            Self::Resident(entry) => entry.gesture.as_deref(),
            Self::Native(entry) => entry.gesture(),
        }
    }
    pub(crate) fn revision(self) -> u64 {
        match self {
            Self::Resident(entry) => entry.after.revision,
            Self::Native(entry) => entry.revision(),
        }
    }
}
impl<'a> Iterator for HistoryEntries<'a> {
    type Item = HistoryEntry<'a>;
    fn next(&mut self) -> Option<Self::Item> {
        match self {
            Self::Resident(entries) => entries.next().map(HistoryEntry::Resident),
            Self::Native(entries) => entries.next().map(HistoryEntry::Native),
        }
    }
    fn size_hint(&self) -> (usize, Option<usize>) {
        match self {
            Self::Resident(entries) => entries.size_hint(),
            Self::Native(entries) => entries.size_hint(),
        }
    }
}
impl DoubleEndedIterator for HistoryEntries<'_> {
    fn next_back(&mut self) -> Option<Self::Item> {
        match self {
            Self::Resident(entries) => entries.next_back().map(HistoryEntry::Resident),
            Self::Native(entries) => entries.next_back().map(HistoryEntry::Native),
        }
    }
}
impl ExactSizeIterator for HistoryEntries<'_> {}
impl NativeCapture {
    pub(crate) fn cancellation(&self) -> native::Cancellation {
        self.edit.cancellation()
    }
    pub(crate) fn document(&self) -> &native::NativeDocument {
        &self.document
    }
    pub(crate) fn into_parts(
        self,
    ) -> (
        native::NativeDocument,
        native::HistorySnapshot,
        native::CapturedEdit,
    ) {
        (self.document, self.history, self.edit)
    }
}
impl DocumentAuthority {
    pub(crate) fn resident_state(&self) -> Result<&ResidentAuthority> {
        match self {
            Self::Resident(state) => Ok(state),
            Self::Native(_) => {
                Err("This operation requires a prepared native document route".into())
            }
        }
    }
    pub(crate) fn resident_state_mut(&mut self) -> Result<&mut ResidentAuthority> {
        match self {
            Self::Resident(state) => {
                // Some resident metadata changes do not advance the edit revision.
                state.snapshot.take();
                Ok(state)
            }
            Self::Native(_) => {
                Err("This operation requires a prepared native document route".into())
            }
        }
    }
    pub(crate) fn into_resident(self) -> Result<ResidentAuthority> {
        match self {
            Self::Resident(state) => Ok(state),
            Self::Native(_) => {
                Err("Native document authority cannot become a resident copy".into())
            }
        }
    }
    pub(crate) fn capture_scene(
        &self,
        preview: Option<&Document>,
        project_id: String,
        leases: Vec<Lease>,
        file_version: Option<crate::file_safety::Version>,
        file_ownership: Option<Arc<crate::file_ownership::Ownership>>,
    ) -> Result<super::scene::CapturedScene> {
        match self {
            Self::Resident(state) => {
                let document = preview.unwrap_or(&state.document);
                if document.id != state.document.id || document.revision != state.document.revision
                {
                    return Err("Preview does not belong to the captured project revision".into());
                }
                Ok(super::scene::CapturedScene::from_parts(
                    document.clone(),
                    project_id,
                    leases,
                    file_version,
                    file_ownership,
                ))
            }
            Self::Native(state) => {
                if preview.is_some() {
                    return Err(
                        "Native previews require their captured prepared-output envelope".into(),
                    );
                }
                let metadata = state.document.metadata();
                let scene = super::scene::SceneSnapshot::from_native(
                    super::scene::SceneIdentity {
                        project_id,
                        document_id: metadata.id().to_owned(),
                        revision: metadata.revision(),
                    },
                    state.document.clone(),
                    leases,
                    file_version,
                    file_ownership,
                )?;
                super::scene::CapturedScene::from_native(scene)
            }
        }
    }
    pub(crate) fn from_resident(document: Document) -> Self {
        Self::Resident(ResidentAuthority {
            document,
            undo: Vec::new(),
            redo: Vec::new(),
            truncated_tasks: BTreeSet::new(),
            snapshot: OnceLock::new(),
        })
    }
    /// Allocate the history owner before installing a native authority. This
    /// constructor neither replaces a live document nor publishes any sources.
    pub(crate) fn from_native(document: native::NativeDocument, limit: usize) -> Result<Self> {
        let history = native::NativeHistory::new(document.budget(), limit)?;
        Ok(Self::Native(NativeAuthority { document, history, raster_damage: None }))
    }
    pub(crate) fn resident(&self) -> Result<&Document> {
        match self {
            Self::Resident(state) => Ok(&state.document),
            Self::Native(_) => {
                Err("This operation requires a prepared native document route".into())
            }
        }
    }
    pub(crate) fn native(&self) -> Result<&native::NativeDocument> {
        match self {
            Self::Native(state) => Ok(&state.document),
            Self::Resident(_) => Err("This operation requires native document authority".into()),
        }
    }
    pub(crate) fn view(&self) -> view::DocumentView<'_> {
        match self {
            Self::Resident(state) => view::DocumentView::Resident(&state.document),
            Self::Native(state) => view::DocumentView::Native(&state.document),
        }
    }
    pub(crate) fn snapshot(&self) -> view::DocumentSnapshot {
        match self {
            Self::Resident(state) => view::DocumentSnapshot::Resident(
                state.snapshot.get_or_init(|| Arc::new(state.document.clone())).clone(),
            ),
            Self::Native(state) => state.document.clone().into(),
        }
    }
    pub(crate) fn task_complete(&self, owner: &str, task: &str) -> bool {
        match self {
            Self::Resident(state) => !state
                .truncated_tasks
                .iter()
                .any(|(a, t)| a == owner && t == task),
            Self::Native(state) => state.history.task_complete(owner, task),
        }
    }
    pub(crate) fn history_lengths(&self) -> [usize; 2] {
        match self {
            Self::Resident(state) => [state.undo.len(), state.redo.len()],
            Self::Native(state) => [state.history.undo_len(), state.history.redo_len()],
        }
    }
    pub(crate) fn history_entries(&self) -> HistoryEntries<'_> {
        match self {
            Self::Resident(state) => HistoryEntries::Resident(state.undo.iter()),
            Self::Native(state) => HistoryEntries::Native(state.history.entries()),
        }
    }
    pub(crate) fn capture_native(
        &self,
        descriptor: Arc<native::CommandDescriptor>,
        actor: &str,
        leases: &[Lease],
        now: u64,
        closed: bool,
        write_available: bool,
    ) -> Result<NativeCapture> {
        let Self::Native(state) = self else {
            return Err("Native command capture requires native authority".into());
        };
        let edit = state.document.capture_descriptor(
            descriptor,
            actor,
            leases,
            now,
            closed,
            write_available,
        )?;
        Ok(NativeCapture {
            document: state.document.clone(),
            history: state.history.snapshot(),
            edit,
        })
    }
    /// The caller supplies final Engine observations while holding its write
    /// fence. All source I/O and allocation precede this method; returned owners
    /// must be released/retired after unlocking, including refusal paths.
    pub(crate) fn commit_native(
        &mut self,
        gate: native::Publication<'_>,
        prepared: native::PreparedCommit,
    ) -> std::result::Result<native::RetiredHistory, native::RefusedCommit> {
        match self {
            Self::Native(state) => {
                let damage = prepared.raster_damage();
                let retired = state.history.commit(&mut state.document, gate, prepared)?;
                state.raster_damage = damage;
                Ok(retired)
            }
            Self::Resident(_) => Err(prepared.refused("Native publication authority changed")),
        }
    }
    pub(crate) fn native_raster_damage(&self) -> Option<native::worker::RasterDamage> {
        let Self::Native(state) = self else { return None; };
        state.raster_damage.filter(|damage| damage.candidate == state.document.key())
    }
    pub(crate) fn native_history(&self) -> Result<native::HistorySnapshot> {
        match self {
            Self::Native(state) => Ok(state.history.snapshot()),
            Self::Resident(_) => Err("Native history requires native authority".into()),
        }
    }
    pub(crate) fn validate_save_name(&self, prepared: &native::PreparedSaveName) -> Result<()> {
        match self {
            Self::Native(state) => state.history.validate_save_name(&state.document, prepared),
            Self::Resident(_) => Err("Native saved name authority changed".into()),
        }
    }
    pub(crate) fn commit_save_name(
        &mut self,
        prepared: native::PreparedSaveName,
    ) -> std::result::Result<native::NativeDocument, native::RefusedSaveName> {
        match self {
            Self::Native(state) => {
                let old = state.history.commit_save_name(&mut state.document, prepared)?;
                state.raster_damage = None;
                Ok(old)
            }
            Self::Resident(_) => {
                Err(prepared.refused("Native saved name authority changed".into()))
            }
        }
    }
    pub(crate) fn move_native(
        &mut self,
        gate: native::Publication<'_>,
        prepared: native::PreparedMove,
    ) -> std::result::Result<native::NativeDocument, native::RefusedMove> {
        match self {
            Self::Native(state) => {
                let old = state.history.move_cursor(&mut state.document, gate, prepared)?;
                state.raster_damage = None;
                Ok(old)
            }
            Self::Resident(_) => Err(prepared.refused("Native history authority changed")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::large_document::{DiskBudget, ResidentBudget};

    fn blank(pool: &ResidentBudget) -> native::NativeDocument {
        native::NativeDocument::new_blank(
            4,
            2,
            16,
            pool,
            &DiskBudget::new(1024 * 1024, 4).unwrap(),
            &|| Ok(()),
        )
        .unwrap()
    }

    #[test]
    fn resident_snapshots_share_metadata_until_mutation_even_at_the_same_revision() {
        let mut state = DocumentAuthority::from_resident(Document::new(4, 2).unwrap());
        let snapshot = |state: &DocumentAuthority| match state.snapshot() {
            view::DocumentSnapshot::Resident(document) => document,
            view::DocumentSnapshot::Native(_) => panic!("Expected a resident snapshot"),
        };
        let before = snapshot(&state);
        assert!(Arc::ptr_eq(&before, &snapshot(&state)));

        state.resident_state_mut().unwrap().document.name = "Saved name".into();
        let after = snapshot(&state);
        assert!(Arc::ptr_eq(&after, &snapshot(&state)));
        assert_eq!(before.revision, after.revision);
        assert_ne!(before.name, after.name);
        assert_eq!(after.name, "Saved name");
    }
    #[test]
    fn borrowed_routes_do_not_create_resident_native_substitutes() {
        let pool = ResidentBudget::new(8 * 1024 * 1024).unwrap();
        let state = DocumentAuthority::from_native(blank(&pool), 8).unwrap();
        let key = state.native().unwrap().key();
        assert!(state.resident().is_err());
        assert_eq!(state.view().depth(), 16);
        let snapshot = state.snapshot();
        let view::DocumentSnapshot::Native(snapshot) = snapshot else {
            panic!("Native snapshot was materialized as a resident document");
        };
        assert_eq!(snapshot.key(), key);
        assert_eq!(state.history_lengths(), [0, 0]);
        drop(snapshot);
        drop(state);
        assert_eq!(pool.used(), 0);
        let resident = DocumentAuthority::from_resident(Document::new(4, 2).unwrap());
        assert!(resident.native().is_err());
        assert_eq!(resident.resident().unwrap().width, 4);
    }
    #[test]
    fn captured_native_commit_and_cursor_use_one_authority_and_retain_refusals() {
        let pool = ResidentBudget::new(16 * 1024 * 1024).unwrap();
        let mut state = DocumentAuthority::from_native(blank(&pool), 8).unwrap();
        let layer = state.view().layer(0).unwrap().id().to_string();
        let before = state.native().unwrap().key();
        let descriptor = native::CommandDescriptor::from_normalized(
            state.native().unwrap(),
            0,
            &[serde_json::json!({"op":"layer.update","layer":layer,"visible":false})],
            &|| Ok(()),
        )
        .unwrap();
        assert!(state
            .capture_native(descriptor.clone(), "human", &[], 0, true, true)
            .is_err());
        assert!(state
            .capture_native(descriptor.clone(), "human", &[], 0, false, false)
            .is_err());
        let captured = state
            .capture_native(descriptor, "human", &[], 0, false, true)
            .unwrap();
        assert_eq!(captured.document().key(), before);
        let (document, history, edit) = captured.into_parts();
        let delta = edit.visibility(&layer, false, &|| Ok(())).unwrap();
        let prepared = history
            .prepare_commit(
                &document,
                delta,
                native::HistoryRequest {
                    actor: "human",
                    label: "Visibility",
                    task: None,
                    gesture: None,
                },
                &|| Ok(()),
            )
            .unwrap();
        let rejected = state
            .commit_native(
                native::Publication {
                    actor: "foreign",
                    leases: &[],
                    now: 0,
                    closed: false,
                    write_available: true,
                },
                prepared,
            )
            .unwrap_err();
        assert_eq!(state.native().unwrap().key(), before);
        assert_eq!(state.history_lengths(), [0, 0]);
        let delta = rejected.into_prepared_delta();
        let prepared = history
            .prepare_commit(
                &document,
                delta,
                native::HistoryRequest {
                    actor: "human",
                    label: "Visibility",
                    task: None,
                    gesture: None,
                },
                &|| Ok(()),
            )
            .unwrap();
        let retired = state
            .commit_native(
                native::Publication {
                    actor: "human",
                    leases: &[],
                    now: 0,
                    closed: false,
                    write_available: true,
                },
                prepared,
            )
            .unwrap();
        assert!(!state.view().layer(0).unwrap().visible());
        assert_eq!(state.history_lengths(), [1, 0]);
        drop(retired);
        let move_plan = state
            .native_history()
            .unwrap()
            .prepare_move(
                state.native().unwrap(),
                native::Direction::Undo,
                "human",
                &|| Ok(()),
            )
            .unwrap();
        let retired = state
            .move_native(
                native::Publication {
                    actor: "human",
                    leases: &[],
                    now: 0,
                    closed: false,
                    write_available: true,
                },
                move_plan,
            )
            .unwrap();
        assert!(state.view().layer(0).unwrap().visible());
        assert_eq!(state.history_lengths(), [0, 1]);
        drop(retired);
        drop(history);
        drop(document);
        drop(state);
        assert_eq!(pool.used(), 0);
    }

    #[test]
    fn live_native_capture_refuses_exhausted_header_budget_without_changing_authority() {
        let pool = ResidentBudget::new(8 * 1024 * 1024).unwrap();
        let mut engine = Engine::from_native(blank(&pool), 8).unwrap();
        let key = engine.native_document().unwrap().key();
        let lease = engine
            .reserve("worker", "Keep captured reservations", vec![])
            .unwrap();
        let baseline = pool.used();
        let pressure = pool
            .reserve_bytes(pool.limit() - baseline, pool.limit())
            .unwrap();
        assert!(engine.capture_scene(None).is_err());
        assert!(engine.capture_state().is_err());
        assert_eq!(engine.native_document().unwrap().key(), key);
        assert_eq!(engine.history_lengths(), [0, 0]);
        assert_eq!(engine.leases[0].id, lease.id);
        assert_eq!(pool.used(), pool.limit());
        drop(pressure);
        assert_eq!(pool.used(), baseline);
        let scene = engine.capture_scene(None).unwrap().into_native().unwrap();
        assert_eq!(scene.key(), key);
        assert_eq!(scene.leases()[0].id, lease.id);
        drop(scene);
        assert_eq!(pool.used(), baseline);
        drop(engine);
        assert_eq!(pool.used(), 0);
    }

    #[test]
    fn live_native_engine_shares_edits_history_reports_and_rendering_without_resident_fallback() {
        use crate::{server, workspace};
        use serde_json::json;
        use std::sync::Mutex;
        for depth in [8, 16] {
            let fixture = native::worker::tests::Fixture::new();
            let shared = Arc::new(Mutex::new(Engine::new()));
            let connection = server::start(shared.clone(), fixture.path().join("state")).unwrap();
            connection.configure_native_sources(None, true).unwrap();
            let runtime = workspace::native_dispatch::runtime_for(&shared).unwrap();
            let pool = runtime.resident();
            let document =
                native::NativeDocument::new_blank(4, 2, depth, &pool, &runtime.disk(), &|| Ok(()))
                    .unwrap();
            let id = document.metadata().id().to_owned();
            let layer = document.metadata().layer(0).unwrap().id().to_owned();
            // Fixture-only adoption preserves the authenticated workspace/runtime.
            // Production adoption awaits connected native save/recovery producers.
            shared.lock().unwrap().authority = Engine::from_native(document, 8).unwrap().authority;
            let captured = {
                let mut engine = shared.lock().unwrap();
                assert!(engine.resident_state().is_err());
                assert!(engine.resident_state_mut().is_err());
                engine.capture_state().unwrap()
            };
            let reply = server::edit_shared(
                &shared, "worker",
                &[
                    json!({"op":"layer.update","layer":layer,"name":"Native batch"}),
                    json!({"op":"paint.fill","layer":layer,"rect":[0,0,4,2],"color":[20,101,201,255]}),
                ],
                Some(0), None, "Native batch", None, Some(&id),
            ).unwrap();
            assert_eq!(reply.data()["revision"], 1);
            drop(reply);
            let old = captured.project().unwrap();
            assert_eq!(old["document"]["revision"], 0);
            assert_eq!(old["document"]["bit_depth"], depth);
            assert_eq!(old["layers"][0]["name"], "Paint 1");
            drop(old);
            let baseline = pool.used();
            let report = server::state(&shared).unwrap();
            assert!(pool.used() > baseline);
            assert_eq!(report["document"]["id"], id);
            assert_eq!(report["document"]["revision"], 1);
            assert_eq!(report["document"]["bit_depth"], depth);
            assert_eq!(report["layers"][0]["name"], "Native batch");
            assert_eq!(report["ai_change"]["actor"], "worker");
            drop(report);
            assert_eq!(pool.used(), baseline);
            for (direction, revision, lengths, expected) in [
                (native::Direction::Undo, 2, [0, 1], [0; 4]),
                (
                    native::Direction::Redo,
                    3,
                    [1, 0],
                    [20 * 257, 101 * 257, 201 * 257, 65535],
                ),
            ] {
                let reply = server::history_shared(
                    &shared,
                    "worker",
                    direction,
                    Some(revision - 1),
                    Some(&id),
                )
                .unwrap();
                assert_eq!(reply.data()["revision"], revision);
                drop(reply);
                let document = {
                    let engine = shared.lock().unwrap();
                    assert!(engine.resident().is_err());
                    assert_eq!(engine.history_lengths(), lengths);
                    engine.native_document().unwrap().clone()
                };
                assert_eq!(document.metadata().depth(), depth);
                let pixels = document
                    .source(document.layer_slot(&layer).unwrap())
                    .unwrap()
                    .prepare_region(
                        crate::raster::backed::Region::new([0, 0, 4, 2]).unwrap(),
                        &mut || Ok(()),
                    )
                    .unwrap();
                assert_eq!(pixels.pixel16(1, 1).unwrap(), expected);
            }
            let scene = Arc::new(
                shared
                    .lock()
                    .unwrap()
                    .capture_scene(None)
                    .unwrap()
                    .into_native()
                    .unwrap(),
            );
            let session = workspace::native_dispatch::begin_read_scene(&shared, &scene).unwrap();
            let request = crate::render::native_preview::Request::capture(
                scene, session, None, 32, None, false,
            )
            .unwrap();
            let frame = request.render(&mut || Ok(())).unwrap();
            assert_eq!(
                &frame.copy_rgba8(&mut || Ok(())).unwrap()[..4],
                &[20, 101, 201, 255]
            );
            drop(frame);
            drop(request);
            let lease = shared
                .lock()
                .unwrap()
                .reserve(
                    "other",
                    "Retain human work",
                    vec![super::super::Scope::layer(&layer)],
                )
                .unwrap();
            let key = shared.lock().unwrap().native_document().unwrap().key();
            assert!(server::edit_shared(
                &shared,
                "worker",
                &[json!({"op":"layer.update","layer":layer,"name":"Refused"})],
                Some(3),
                None,
                "Refused",
                None,
                Some(&id),
            )
            .is_err());
            {
                let mut engine = shared.lock().unwrap();
                assert_eq!(engine.native_document().unwrap().key(), key);
                assert_eq!(engine.history_lengths(), [1, 0]);
                assert_eq!(
                    engine
                        .file_operations
                        .load(std::sync::atomic::Ordering::Acquire),
                    0
                );
                engine.finish_task(&lease.id, "ended");
            }
            drop(shared);
            drop(runtime);
            drop(connection);
        }
    }
}
