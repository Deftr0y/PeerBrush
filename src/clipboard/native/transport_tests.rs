use super::*;
use crate::clipboard::{EditRequest, ImageIdentity, LayerSnapshot, Provider, Session as ClipboardSession};
use crate::engine::{view::DocumentSnapshot, Engine};
use std::sync::Mutex;

#[test]
fn observations_distinguish_native_generations_storage_and_revision() {
    let first = Fixture::new(doc(16, 0));
    let second = Fixture::new(first.legacy.clone());
    let snapshot = DocumentSnapshot::Native(first.document.clone());
    let clone = snapshot.clone();
    assert!(snapshot.view().same_observation(clone.view()));
    assert!(!snapshot.view().same_observation((&second.document).into()));
    assert!(!snapshot.view().same_observation((&first.legacy).into()));
    let resident = DocumentSnapshot::from(first.legacy.clone());
    assert!(resident.view().same_observation((&first.legacy).into()));
    let mut newer = first.legacy.clone();
    newer.revision += 1;
    assert!(!resident.view().same_observation((&newer).into()));
    drop((snapshot, clone));
    first.finish();
    second.finish();
}

#[derive(Default)]
struct Marker {
    text: Option<String>,
    fail: bool,
}
impl Provider for Marker {
    fn write_image(
        &mut self,
        _: &crate::clipboard::Image,
        _: &dyn Fn() -> Result<(), String>,
    ) -> Result<Option<ImageIdentity>, String> {
        panic!("Whole-layer copies must not materialize an OS bitmap")
    }
    fn read_image(&mut self) -> Result<(crate::clipboard::ClipboardImage, Option<ImageIdentity>), String> {
        panic!("Whole-layer copies must not decode an OS bitmap")
    }
    fn write_text(&mut self, text: &str) -> Result<(), String> {
        if self.fail {
            return Err("marker write refused".into());
        }
        self.text = Some(text.into());
        Ok(())
    }
    fn read_text(&mut self) -> Result<String, String> {
        self.text.clone().ok_or_else(|| "no marker".into())
    }
}

#[test]
fn native_layer_capture_keeps_source_owners_and_failed_marker_preserves_previous_copy() {
    for depth in [8, 16] {
        let fixture = Fixture::new(doc(depth, 3));
        let hash = fixture.source_hash();
        let disk = fixture.disk.used().unwrap();
        let mut session = ClipboardSession::default();
        let mut marker = Marker::default();
        let request = || EditRequest::Layers {
            doc: DocumentSnapshot::Native(fixture.document.clone()),
            ids: vec![fixture.legacy.layers[0].id.clone()],
            cut: None,
        };
        let reply = session.process_edit(&mut marker, request());
        assert_eq!(reply.document, fixture.legacy.id);
        assert_eq!(reply.result.unwrap(), "Layers copied");
        let old_marker = marker.text.clone();
        let buffer = session.layers.as_ref().unwrap();
        assert_eq!(Some(&buffer.marker), old_marker.as_ref());
        let LayerSnapshot::Native(copy) = &buffer.layers else {
            panic!("Native clipboard became a resident copy")
        };
        assert_eq!(copy.key(), fixture.document.key());
        assert_eq!(copy.members(), &[0]);
        copy.source()
            .metadata()
            .visit_rasters(|raster| {
                assert_eq!(
                    copy.source().source(raster.slot())?.key(),
                    fixture.document.source(raster.slot())?.key(),
                );
                Ok(())
            })
            .unwrap();
        marker.fail = true;
        assert!(session.process_edit(&mut marker, request()).result.is_err());
        assert_eq!(marker.text, old_marker);
        assert_eq!(
            Some(&session.layers.as_ref().unwrap().marker),
            old_marker.as_ref()
        );
        assert_eq!(fixture.disk.used().unwrap(), disk);
        assert_eq!(fixture.source_hash(), hash);
        drop(session);
        fixture.finish();
    }
}

#[test]
fn native_cut_checks_locks_and_storage_generation_before_deleting_any_work() {
    for refusal in ["locked", "read-only", "authority"] {
        let mut source = doc(16, 0);
        if refusal == "locked" {
            source.layers[0].locked = true;
        }
        if refusal == "read-only" {
            source.read_only = true;
        }
        let fixture = Fixture::new(source);
        let hash = fixture.source_hash();
        let mut engine = Engine::new();
        // The identity/revision deliberately match; the native generation does not.
        engine.resident_state_mut().expect("resident fixture").document = fixture.legacy.clone();
        let before = serde_json::to_value(&engine.resident_state().expect("resident fixture").document).unwrap();
        let shared = Arc::new(Mutex::new(engine));
        let mut session = ClipboardSession::default();
        let mut marker = Marker::default();
        let reply = session.process_edit(
            &mut marker,
            EditRequest::Layers {
                doc: fixture.document.clone().into(),
                ids: vec![fixture.legacy.layers[0].id.clone()],
                cut: Some(shared.clone()),
            },
        );
        let error = reply.result.unwrap_err();
        if refusal == "authority" {
            assert!(error.contains("Observed edit source changed"));
            assert!(error.contains("Layers were copied; original layers remain unchanged"));
            assert!(marker.text.is_some());
            assert!(matches!(
                session.layers.as_ref().unwrap().layers,
                LayerSnapshot::Native(_)
            ));
        } else {
            assert!(marker.text.is_none());
            assert!(session.layers.is_none());
        }
        let engine = shared.lock().unwrap();
        assert_eq!(serde_json::to_value(&engine.resident_state().expect("resident fixture").document).unwrap(), before);
        assert_eq!(engine.history_lengths(), [0, 0]);
        assert_eq!(
            engine
                .file_operations
                .load(std::sync::atomic::Ordering::Acquire),
            0
        );
        assert_eq!(fixture.source_hash(), hash);
        drop((engine, session));
        fixture.finish();
    }
}

#[test]
fn protected_sources_require_explicit_compatibility_conversion_before_layer_copy() {
    let mut source = doc(16, 0);
    source.read_only = true;
    let fixture = Fixture::new(source);
    let hash = fixture.source_hash();
    let ids = vec![fixture.legacy.layers[0].id.clone()];
    let resident = crate::layer_clipboard::copy(&fixture.legacy, &ids).unwrap_err();
    assert!(resident.contains("explicit compatible copy"));
    let mut session = ClipboardSession::default();
    let mut marker = Marker::default();
    let reply = session.process_edit(
        &mut marker,
        EditRequest::Layers {
            doc: fixture.document.clone().into(),
            ids,
            cut: None,
        },
    );
    assert_eq!(reply.result.unwrap_err(), resident);
    assert!(marker.text.is_none());
    assert!(session.layers.is_none());
    assert_eq!(fixture.source_hash(), hash);
    fixture.finish();
}

#[test]
fn clipboard_worker_retains_native_copy_and_accepts_legacy_requests_afterward() {
    let fixture = Fixture::new(doc(16, 0));
    let mut engine = Engine::new();
    engine.resident_state_mut().expect("resident fixture").document = fixture.legacy.clone();
    let before = serde_json::to_value(&engine.resident_state().expect("resident fixture").document).unwrap();
    let shared = Arc::new(Mutex::new(engine));
    let worker = crate::clipboard::Worker::memory(eframe::egui::Context::default());
    let target = fixture.legacy.layers[0].id.clone();
    worker
        .copy_layers(fixture.document.clone().into(), vec![target.clone()], None)
        .unwrap();
    let reply = worker
        .replies
        .recv_timeout(std::time::Duration::from_secs(5))
        .unwrap();
    assert_eq!(reply.document, fixture.legacy.id);
    assert_eq!(reply.result.unwrap(), "Layers copied");
    worker
        .request(crate::clipboard::Request::Paste {
            shared: shared.clone(),
            document: fixture.legacy.id.clone(),
            revision: fixture.legacy.revision,
            target: target.clone(),
        })
        .unwrap();
    let reply = worker
        .replies
        .recv_timeout(std::time::Duration::from_secs(5))
        .unwrap();
    assert!(reply
        .result
        .unwrap_err()
        .contains("cannot be pasted into a resident document"));
    assert_eq!(
        serde_json::to_value(&shared.lock().unwrap().resident_state().expect("resident fixture").document).unwrap(),
        before
    );
    // Retire the native clipboard owner before fixture teardown, on the same worker.
    worker
        .request(crate::clipboard::Request::CopyLayers {
            doc: fixture.legacy.clone(),
            ids: vec![target],
        })
        .unwrap();
    assert!(worker
        .replies
        .recv_timeout(std::time::Duration::from_secs(5))
        .unwrap()
        .result
        .is_ok());
    drop(worker);
    fixture.finish();
}
