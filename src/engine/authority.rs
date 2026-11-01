//! Exclusive document and history ownership. Callers inspect borrowed views;
//! native preparation and post-publication retirement happen outside Engine locks.
use super::{native, view, Document, Engine, History, Lease};
use std::{collections::BTreeSet, sync::Arc};

type Result<T> = std::result::Result<T, String>;

/// Immutable preparation identity, captured from live Engine authority only.
/// It is neither an editable document nor filesystem publication permission.
pub(crate) struct NativeSeed {
    document: native::NativeDocument,
    project_id: String,
    path: Option<std::path::PathBuf>,
    file_version: Option<crate::file_safety::Version>,
    ownership: Option<Arc<crate::file_ownership::Ownership>>,
    _charge: Arc<crate::psd::memory36::MetadataCharge>,
}
impl NativeSeed {
    pub(crate) fn document(&self) -> &native::NativeDocument {
        &self.document
    }
    pub(crate) fn project_id(&self) -> &str {
        &self.project_id
    }
    fn capture(engine: &Engine, document: &native::NativeDocument) -> Result<Self> {
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
            path,
            file_version: engine.file_version,
            ownership: engine.file_ownership.clone(),
            _charge: charge,
        })
    }
    fn validate(&self, engine: &Engine) -> Result<()> {
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
    // The sole authority field binds capture, snapshots, history and final
    // publication. A native route never derives pixels from resident state.
    fn native_authority(&self) -> Option<&DocumentAuthority> {
        Some(&self.authority)
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
}

pub(crate) enum DocumentAuthority {
    Resident(ResidentAuthority),
    Native(NativeAuthority),
}
pub(crate) struct ResidentAuthority {
    pub(crate) document: Document,
    pub(crate) undo: Vec<History>,
    pub(crate) redo: Vec<History>,
    pub(crate) truncated_tasks: BTreeSet<(String, String)>,
}
pub(crate) struct NativeAuthority {
    pub(super) document: native::NativeDocument,
    history: native::NativeHistory,
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
    pub(crate) fn capture_scene(
        &self,
        preview: Option<&Document>,
        project_id: &str,
        leases: &[Lease],
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
                    project_id.to_owned(),
                    leases.to_vec(),
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
                let scene = super::scene::SceneSnapshot::capture_native(
                    project_id,
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
        })
    }
    /// Allocate the history owner before installing a native authority. This
    /// constructor neither replaces a live document nor publishes any sources.
    pub(crate) fn from_native(document: native::NativeDocument, limit: usize) -> Result<Self> {
        let history = native::NativeHistory::new(document.budget(), limit)?;
        Ok(Self::Native(NativeAuthority { document, history }))
    }
    pub(crate) fn resident(&self) -> Result<&Document> {
        match self {
            Self::Resident(state) => Ok(&state.document),
            Self::Native(_) => {
                Err("This operation requires a prepared native document route".into())
            }
        }
    }
    pub(crate) fn resident_state(&self) -> Result<&ResidentAuthority> {
        match self {
            Self::Resident(state) => Ok(state),
            Self::Native(_) => Err("This operation requires its prepared native route".into()),
        }
    }
    pub(crate) fn resident_state_mut(&mut self) -> Result<&mut ResidentAuthority> {
        match self {
            Self::Resident(state) => Ok(state),
            Self::Native(_) => Err("This operation requires its prepared native route".into()),
        }
    }
    pub(super) fn into_resident_document(self) -> Result<Document> {
        match self {
            Self::Resident(state) => Ok(state.document),
            Self::Native(_) => Err("Native authority cannot become a resident document".into()),
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
            Self::Resident(state) => state.document.clone().into(),
            Self::Native(state) => state.document.clone().into(),
        }
    }
    pub(crate) fn task_complete(&self, owner: &str, task: &str) -> bool {
        match self {
            Self::Resident(state) => !state
                .truncated_tasks
                .contains(&(owner.to_owned(), task.to_owned())),
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
            Self::Native(state) => state.history.commit(&mut state.document, gate, prepared),
            Self::Resident(_) => Err(prepared.refused("Native publication authority changed")),
        }
    }
    pub(crate) fn native_history(&self) -> Result<native::HistorySnapshot> {
        match self {
            Self::Native(state) => Ok(state.history.snapshot()),
            Self::Resident(_) => Err("Native history requires native authority".into()),
        }
    }
    pub(crate) fn move_native(
        &mut self,
        gate: native::Publication<'_>,
        prepared: native::PreparedMove,
    ) -> std::result::Result<native::NativeDocument, native::RefusedMove> {
        match self {
            Self::Native(state) => state
                .history
                .move_cursor(&mut state.document, gate, prepared),
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
    fn live_native_engine_uses_one_document_history_and_snapshot_generation() {
        let pool = ResidentBudget::new(8 * 1024 * 1024).unwrap();
        let mut engine = Engine::new_native(blank(&pool)).unwrap();
        assert!(engine.resident().is_err());
        let seed = engine.native_seed().unwrap().unwrap();
        let key = seed.document().key();
        assert_eq!(
            engine.doc_snapshot().view().id(),
            seed.document().metadata().id()
        );
        assert_eq!(
            engine.doc_snapshot().view().revision(),
            seed.document().metadata().revision()
        );
        assert!(engine.state().is_err());
        let layer = seed.document().metadata().layer(0).unwrap().id().to_owned();
        let descriptor = native::CommandDescriptor::from_normalized(
            seed.document(),
            0,
            &[serde_json::json!({"op":"layer.update","layer":layer,"visible":false})],
            &|| Ok(()),
        )
        .unwrap();
        let capture = engine
            .capture_native(
                &seed,
                descriptor.clone(),
                "human",
                Some(0),
                Some(seed.document().metadata().id()),
            )
            .unwrap();
        let (document, history, edit) = capture.into_parts();
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
        engine.closed = true;
        let refused = engine.commit_native(&seed, "human", prepared).unwrap_err();
        assert_eq!(engine.native_document().unwrap().key(), key);
        assert_eq!(engine.history_lengths(), [0, 0]);
        engine.closed = false;
        let prepared = history
            .prepare_commit(
                &document,
                refused.into_prepared_delta(),
                native::HistoryRequest {
                    actor: "human",
                    label: "Visibility",
                    task: None,
                    gesture: None,
                },
                &|| Ok(()),
            )
            .unwrap();
        let retired = engine.commit_native(&seed, "human", prepared).unwrap();
        assert!(!engine
            .native_document()
            .unwrap()
            .metadata()
            .layer(0)
            .unwrap()
            .visible());
        assert_eq!(engine.history_lengths(), [1, 0]);
        assert_eq!(engine.doc_snapshot().view().revision(), 1);
        assert!(engine
            .capture_native(&seed, descriptor, "human", Some(0), None)
            .is_err());
        drop((retired, history, document, seed, engine));
        assert_eq!(pool.used(), 0);
    }
    #[test]
    fn live_native_factory_keeps_current_geometry_caps_closed() {
        let pool = ResidentBudget::new(8 * 1024 * 1024).unwrap();
        let disk = DiskBudget::new(1024 * 1024, 4).unwrap();
        let document =
            native::NativeDocument::new_blank(16000, 16000, 16, &pool, &disk, &|| Ok(())).unwrap();
        assert!(Engine::new_native(document).is_err());
        assert_eq!(pool.used(), 0);
        assert_eq!(disk.used().unwrap(), 0);
    }
}
