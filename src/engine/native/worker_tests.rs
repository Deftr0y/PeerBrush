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

pub(crate) struct Fixture {
    path: PathBuf,
    held: Option<Directory>,
}
impl Fixture {
    pub(crate) fn new() -> Self {
        let path = fs::canonicalize(std::env::temp_dir())
            .unwrap()
            .join(format!("pb-native-worker-{}", crate::engine::id()));
        fs::create_dir(&path).unwrap();
        Self {
            held: Some(Directory::open(&path).unwrap()),
            path,
        }
    }
    pub(crate) fn runtime(&self) -> (Arc<SourceRuntime>, crate::server::Connection) {
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
pub(crate) fn capture(state: &DocumentAuthority, commands: &[serde_json::Value]) -> NativeCapture {
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
    for (second, expected_reason) in [
        (
            serde_json::json!({"op":"source.update","layer":layer,"opacity":2}),
            "Native command has no connected bounded producer",
        ),
        (
            serde_json::json!({"op":"layer.update","layer":layer,"blend":"@unsupported-blend"}),
            "Unsupported blend mode",
        ),
    ] {
        let commands = [
            serde_json::json!({"op":"layer.update","layer":layer,"visible":false}),
            second,
        ];
        let failure = match prepare(capture(&state, &commands), &session, &|| Ok(())) {
            Err(failure) => failure,
            Ok(prepared) => {
                prepared
                    .abort(&session, &|| session.cleanup_check())
                    .unwrap();
                panic!("Refused command unexpectedly prepared");
            }
        };
        assert_eq!(failure.reason, expected_reason);
        assert!(failure.cleanup_error.is_none());
        assert_eq!(state.native().unwrap().key(), key);
        assert!(state.view().layer(0).unwrap().visible());
        assert_eq!(state.history_lengths(), [0, 0]);
        assert_eq!(session.disk().used().unwrap(), disk);
    }
    // Out-of-range opacity is a supported legacy clamp, not invalid input.
    let clamped = prepare(
        capture(
            &state,
            &[
                serde_json::json!({"op":"layer.update","layer":layer,"visible":false}),
                serde_json::json!({"op":"layer.update","layer":layer,"opacity":2}),
            ],
        ),
        &session,
        &|| Ok(()),
    )
    .unwrap();
    assert_eq!(clamped.preview().metadata().layer(0).unwrap().opacity(), 1.);
    assert!(!clamped.preview().metadata().layer(0).unwrap().visible());
    assert_eq!(state.native().unwrap().key(), key);
    assert_eq!(state.history_lengths(), [0, 0]);
    clamped
        .abort(&session, &|| session.cleanup_check())
        .unwrap();
    assert_eq!(session.disk().used().unwrap(), disk);
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

#[test]
fn shared_worker_brush_fill_and_clear_match_legacy_native_pixels_at_both_depths() {
    let fixture = Fixture::new();
    let (runtime, connection) = fixture.runtime();
    let session = runtime.begin().unwrap();
    for depth in [8, 16] {
        let doc = NativeDocument::new_blank(
            4,
            2,
            depth,
            &runtime.resident(),
            &runtime.disk(),
            &|| Ok(()),
        )
        .unwrap();
        let mut state = DocumentAuthority::from_native(doc, 8).unwrap();
        let layer = state.view().layer(0).unwrap().id().to_owned();
        let mut legacy = crate::engine::Engine::new();
        legacy.doc = crate::engine::Document::new_depth(4, 2, depth).unwrap();
        let commands = [
            serde_json::json!({"op":"paint","layer":layer,"points":[[0.5,0.5,0.4],[3.5,1.5,0.8]],"radius":0.8,"hardness":0.3,"flow":0.8,"opacity":0.6,"color":[239,39,71,233]}),
            serde_json::json!({"op":"paint.fill","layer":layer,"rect":[1,0,3,2],"color":[20,101,201,177]}),
        ];
        let mut oracle = commands.to_vec();
        for command in &mut oracle {
            command["layer"] = serde_json::json!(legacy.doc.layers[0].id);
        }
        legacy
            .edit("human", &oracle, Some(0), None, "Paint")
            .unwrap();
        let prepared = prepare(capture(&state, &commands), &session, &|| Ok(())).unwrap();
        let compare = |native: &NativeDocument, legacy: &crate::engine::Document| {
            let pixels = native
                .source(native.metadata().layer(0).unwrap().pixels().slot())
                .unwrap();
            let region = pixels
                .prepare_region(
                    crate::raster::backed::Region::new([0, 0, 4, 2]).unwrap(),
                    &mut || Ok(()),
                )
                .unwrap();
            for y in 0..2 {
                for x in 0..4 {
                    assert_eq!(
                        region.pixel16(x, y).unwrap(),
                        legacy.layers[0].pixels.get16(x, y)
                    );
                }
            }
        };
        compare(prepared.preview(), &legacy.doc);
        let (base, history, delta) = prepared.into_parts();
        let commit = history
            .prepare_commit(
                &base,
                delta,
                HistoryRequest {
                    actor: "human",
                    label: "Paint",
                    task: None,
                    gesture: None,
                },
                &|| Ok(()),
            )
            .unwrap();
        state
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
            .unwrap()
            .retire(&session, &|| session.cleanup_check())
            .unwrap();
        assert_eq!(state.history_lengths(), [1, 0]);
        let current = state.native().unwrap().clone();
        drop((base, history, state));
        let selected = current
            .capture_edit()
            .unwrap()
            .batch(
                RasterBatch {
                    rasters: vec![],
                    layers: vec![],
                    effects: vec![],
                    selection: Some(SelectionPatch {
                        bounds: Some([1, 0, 3, 2]),
                        polygon: None,
                        current: None,
                        previous: None,
                    }),
                },
                &|| Ok(()),
            )
            .unwrap_or_else(|refused| panic!("{}", refused.reason()));
        let current = current.adopt(selected).unwrap();
        let state = DocumentAuthority::from_native(current, 8).unwrap();
        legacy.doc.selection = Some([1, 0, 3, 2]);
        legacy.doc.revision = state.view().revision();
        let command = serde_json::json!({"op":"paint.clear_selection","layer":layer});
        legacy.edit("human",&[serde_json::json!({"op":"paint.clear_selection","layer":legacy.doc.layers[0].id})],Some(state.view().revision()),None,"Clear").unwrap();
        let prepared = prepare(capture(&state, &[command]), &session, &|| Ok(())).unwrap();
        compare(prepared.preview(), &legacy.doc);
        prepared
            .abort(&session, &|| session.cleanup_check())
            .unwrap();
        drop(state);
    }
    drop((session, runtime, connection));
}
