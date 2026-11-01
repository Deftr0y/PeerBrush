use super::*;
use crate::{
    engine::authority::DocumentAuthority,
    large_document::root::{Directory, PersistentRootContext},
    workspace::source_runtime::{Limits, SourceRuntime},
};
use std::{
    fs,
    path::PathBuf,
    sync::{Arc, Mutex},
};

struct Fixture {
    path: PathBuf,
    held: Option<Directory>,
}
impl Fixture {
    fn new() -> Self {
        let path = fs::canonicalize(std::env::temp_dir())
            .unwrap()
            .join(format!("pb-native-worker-{}", crate::engine::id()));
        fs::create_dir(&path).unwrap();
        Self {
            held: Some(Directory::open(&path).unwrap()),
            path,
        }
    }
    fn runtime(&self) -> (Arc<SourceRuntime>, crate::server::Connection) {
        let connection = crate::server::start(
            Arc::new(Mutex::new(crate::engine::Engine::new())),
            self.path.join("state"),
        )
        .unwrap();
        let runtime = SourceRuntime::new(
            PersistentRootContext::from_explicit_state(&connection).unwrap(),
            Limits {
                resident_bytes: 32 * 1024 * 1024,
                disk_bytes: 8 * 1024 * 1024,
                max_files: 16,
                store: crate::large_document::Limits {
                    disk_bytes: 4 * 1024 * 1024,
                    max_blocks: 32,
                    max_resident_blocks: 8,
                    manifest_bytes: 65536,
                },
            },
        )
        .unwrap();
        (runtime, connection)
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        assert!(self.held.as_ref().unwrap().check().is_ok());
        assert_eq!(fs::canonicalize(&self.path).unwrap(), self.path);
        assert_eq!(
            self.path.parent(),
            Some(fs::canonicalize(std::env::temp_dir()).unwrap().as_path())
        );
        assert!(self
            .path
            .file_name()
            .unwrap()
            .to_string_lossy()
            .starts_with("pb-native-worker-"));
        drop(self.held.take());
        fs::remove_dir_all(&self.path).unwrap();
    }
}
fn capture(state: &DocumentAuthority, commands: &[serde_json::Value]) -> NativeCapture {
    let document = state.native().unwrap();
    let descriptor = CommandDescriptor::from_normalized(
        document,
        document.metadata().revision(),
        commands,
        &|| Ok(()),
    )
    .unwrap();
    state
        .capture_native(descriptor, "human", &[], 0, false, true)
        .unwrap()
}
#[test]
fn actual_worker_preview_abort_and_commit_use_the_same_atomic_metadata_batch() {
    let fixture = Fixture::new();
    let (runtime, connection) = fixture.runtime();
    let session = runtime.begin().unwrap();
    let document =
        NativeDocument::new_blank(4, 2, 16, &runtime.resident(), &runtime.disk(), &|| Ok(()))
            .unwrap();
    let mut state = DocumentAuthority::from_native(document, 8).unwrap();
    let key = state.native().unwrap().key();
    let layer = state.view().layer(0).unwrap().id().to_owned();
    let commands = [
        serde_json::json!({"op":"layer.update","layer":layer,"name":"Batch","source_revision":0}),
        serde_json::json!({"op":"layer.update","layer":layer,"visible":false,"source_revision":0}),
    ];
    let disk = session.disk().used().unwrap();
    let preview = prepare(capture(&state, &commands), &session, &|| Ok(())).unwrap();
    let base_scene = state
        .capture_scene(
            None,
            "worker-preview".into(),
            vec![crate::engine::Lease {
                id: "lease".into(),
                owner: "human".into(),
                description: "Preview".into(),
                scopes: vec![crate::engine::Scope::layer(&layer)],
                expires: 100,
            }],
            None,
            None,
        )
        .unwrap()
        .into_native()
        .unwrap();
    assert_eq!(preview.base_key(), base_scene.key());
    let candidate_scene = base_scene.with_prepared_preview(&preview).unwrap();
    assert_eq!(candidate_scene.key(), preview.preview().key());
    assert_eq!(
        candidate_scene.identity().project_id,
        base_scene.identity().project_id
    );
    assert_eq!(
        candidate_scene.identity().document_id,
        base_scene.identity().document_id
    );
    assert_eq!(candidate_scene.identity().revision, 1);
    assert_eq!(
        candidate_scene.leases().as_ptr(),
        base_scene.leases().as_ptr()
    );
    assert_eq!(candidate_scene.file_version(), base_scene.file_version());
    // A derived candidate cannot serve as a substitute captured base.
    assert!(candidate_scene.with_prepared_preview(&preview).is_err());
    drop(candidate_scene);
    drop(base_scene);
    assert_eq!(
        preview.preview().metadata().layer(0).unwrap().name(),
        "Batch"
    );
    assert!(!preview.preview().metadata().layer(0).unwrap().visible());
    assert_eq!(preview.preview().metadata().revision(), 1);
    assert_eq!(state.native().unwrap().key(), key);
    assert_eq!(state.history_lengths(), [0, 0]);
    preview
        .abort(&session, &|| session.cleanup_check())
        .unwrap();
    assert_eq!(session.disk().used().unwrap(), disk);
    let prepared = prepare(capture(&state, &commands), &session, &|| Ok(())).unwrap();
    let (base, history, delta) = prepared.into_parts();
    let commit = history
        .prepare_commit(
            &base,
            delta,
            HistoryRequest {
                actor: "human",
                label: "Batch",
                task: None,
                gesture: None,
            },
            &|| Ok(()),
        )
        .unwrap();
    let retired = state
        .commit_native(
            Publication {
                actor: "human",
                leases: &[],
                now: 0,
                closed: false,
                write_available: true,
            },
            commit,
        )
        .unwrap();
    retired
        .retire(&session, &|| session.cleanup_check())
        .unwrap();
    assert_eq!(state.history_lengths(), [1, 0]);
    assert_eq!(state.view().revision(), 1);
    assert_eq!(state.view().layer(0).unwrap().name(), "Batch");
    assert_eq!(session.disk().used().unwrap(), disk);
    drop((history, base, state, session, runtime, connection));
}
#[test]
fn worker_preflight_late_parameter_refusal_and_cancel_preserve_live_authority() {
    let fixture = Fixture::new();
    let (runtime, connection) = fixture.runtime();
    let session = runtime.begin().unwrap();
    let state = DocumentAuthority::from_native(
        NativeDocument::new_blank(4, 2, 8, &runtime.resident(), &runtime.disk(), &|| Ok(()))
            .unwrap(),
        8,
    )
    .unwrap();
    let key = state.native().unwrap().key();
    let layer = state.view().layer(0).unwrap().id().to_owned();
    let disk = session.disk().used().unwrap();
    for operation in ["source.update", "layer.update"] {
        let commands = [
            serde_json::json!({"op":"layer.update","layer":layer,"visible":false}),
            serde_json::json!({"op":operation,"layer":layer,"opacity":2}),
        ];
        assert!(prepare(capture(&state, &commands), &session, &|| Ok(())).is_err());
        assert_eq!(state.native().unwrap().key(), key);
        assert!(state.view().layer(0).unwrap().visible());
        assert_eq!(state.history_lengths(), [0, 0]);
        assert_eq!(session.disk().used().unwrap(), disk);
    }
    let captured = capture(
        &state,
        &[serde_json::json!({"op":"layer.update","layer":layer,"visible":false})],
    );
    captured.cancellation().cancel();
    assert!(prepare(captured, &session, &|| Ok(())).is_err());
    assert_eq!(state.native().unwrap().key(), key);
    assert_eq!(session.disk().used().unwrap(), disk);
    drop((state, session, runtime, connection));
}
