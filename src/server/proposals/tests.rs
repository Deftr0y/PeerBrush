use super::*;
use crate::{
    engine::{
        native::{worker::tests::Fixture, NativeDocument},
        scene::{CapturedScene, LegacyTileCache},
        Document, Engine,
    },
    server::{dispatch, dispatch_owned},
    workspace::{self, source_runtime::SourceRuntime, Registry},
};
use base64::{engine::general_purpose::STANDARD, Engine as _};
use std::sync::{Arc, Mutex};

fn project(runtime: &Arc<SourceRuntime>, depth: u16) -> (Shared, Registry, Document) {
    let mut original = Document::new_depth(19, 13, depth).unwrap();
    original.layers[0]
        .pixels
        .set16(2, 3, [12347, 23459, 34571, 61379]);
    let cache = LegacyTileCache::new(&runtime.resident(), 32).unwrap();
    let staged =
        CapturedScene::from_parts(original.clone(), "protocol-test".into(), vec![], None, None)
            .stage(runtime, &cache, &|| Ok(()))
            .unwrap();
    let native =
        NativeDocument::from_pending(staged.scene.into_provisional_pending().unwrap()).unwrap();
    let shared = Arc::new(Mutex::new(Engine::from_native(native, 8).unwrap()));
    let workspace = workspace::attach(&shared);
    workspace.lock().unwrap().source_runtime = Some(runtime.clone());
    (shared, workspace, original)
}
fn wire(shared: &Shared, request: &Value) -> Value {
    let reply = dispatch_owned(shared, "proposal", request).unwrap();
    serde_json::from_slice(&serde_json::to_vec(&reply).unwrap()).unwrap()
}
fn pixels(reply: &Value) -> Vec<u8> {
    let png = STANDARD
        .decode(reply["images"][0]["data"].as_str().unwrap())
        .unwrap();
    image::load_from_memory(&png).unwrap().to_rgba8().into_raw()
}

#[test]
fn owned_native_proposals_render_frozen_pixels_then_accept_at_the_new_revision() {
    for depth in [8, 16] {
        let fixture = Fixture::new();
        let (runtime, _connection) = fixture.runtime();
        let (shared, workspace, original) = project(&runtime, depth);
        let project = shared.lock().unwrap().project_id.clone();
        let source = shared.lock().unwrap().native_document().unwrap().key();
        let commands = vec![
            json!({"op":"paint.fill","layer":original.layers[0].id,"rect":[4,3,10,9],"color":[31,73,149,177]}),
        ];
        let rect = [3, 2, 12, 10];
        let mut oracle = Engine::from_document(original.clone());
        oracle
            .edit("agent", &commands, Some(0), None, "Review")
            .unwrap();
        let expected = oracle
            .resident_state()
            .unwrap()
            .document
            .preview(Some(rect), 32, None, false)
            .unwrap()
            .2;
        let created = wire(
            &shared,
            &json!({"action":"create","project_id":project,"document_id":original.id,"expected_revision":0,"commands":commands,"rect":rect,"max_edge":32}),
        );
        let id = created["proposal"]["id"].as_str().unwrap();
        assert_eq!(created["proposal"]["status"], "pending");
        assert_eq!(created["images"][0]["document_rect"], json!(rect));
        assert_eq!(created["images"][0]["preview_only"], true);
        assert_eq!(created["images"][0]["revision"], 0);
        assert_eq!(pixels(&created), expected);
        {
            let engine = shared.lock().unwrap();
            assert_eq!(engine.native_document().unwrap().key(), source);
            assert_eq!(engine.history_lengths(), [0, 0]);
        }
        let listing = wire(&shared, &json!({"action":"list"}));
        assert_eq!(listing["proposals"][0]["id"], id);
        assert!(listing["proposals"][0].get("commands").is_none());
        let mut accept = json!({"action":"accept","project_id":project,"document_id":original.id,"expected_revision":0,"proposal":id,"rect":rect,"max_edge":32});
        assert!(dispatch_owned(&shared, "proposal", &accept).is_err());
        accept["actor"] = json!("human");
        let accepted = wire(&shared, &accept);
        assert_eq!(accepted["revision"], 1);
        assert_eq!(accepted["accepted_by"], "human");
        assert!(accepted.get("feedback_warning").is_none());
        assert_eq!(accepted["images"][0]["revision"], 1);
        assert_eq!(pixels(&accepted), expected);
        {
            let engine = shared.lock().unwrap();
            assert_eq!(engine.history_lengths(), [1, 0]);
            assert_eq!(engine.history_entries().next().unwrap().actor(), "agent");
        }
        assert!(dispatch_owned(&shared, "proposal", &accept).is_err());
        native::finish_workspace(&workspace);
    }
}

#[test]
fn detached_transport_invalid_requests_and_stale_acceptance_preserve_native_work() {
    let fixture = Fixture::new();
    let (runtime, _connection) = fixture.runtime();
    let (shared, workspace, original) = project(&runtime, 16);
    let request = json!({"action":"create","document_id":original.id,"expected_revision":0,"feedback":"request","commands":[{"op":"paint.fill","layer":original.layers[0].id,"color":[31,73,149,177]}]});
    assert!(dispatch(&shared, "proposal", &request).is_err());
    let mut invalid = request.clone();
    invalid["rect"] = json!("invalid");
    assert!(dispatch_owned(&shared, "proposal", &invalid).is_err());
    assert!(shared.lock().unwrap().proposals.is_empty());
    let created = wire(&shared, &request);
    let id = &created["proposal"]["id"];
    let stale = json!({"action":"accept","actor":"human","document_id":original.id,"expected_revision":1,"proposal":id,"feedback":"request"});
    assert!(dispatch_owned(&shared, "proposal", &stale).is_err());
    let listing = wire(&shared, &json!({"action":"list"}));
    assert_eq!(listing["proposals"][0]["status"], "pending");
    let reject = json!({"action":"reject","actor":"human","proposal":id});
    assert!(dispatch(&shared, "proposal", &reject).is_err());
    assert_eq!(wire(&shared, &reject)["status"], "rejected");
    assert!(dispatch_owned(
        &shared,
        "proposal",
        &json!({"action":"preview","proposal":id})
    )
    .is_err());
    assert_eq!(shared.lock().unwrap().history_lengths(), [0, 0]);
    native::finish_workspace(&workspace);
}

#[test]
fn native_preset_freezing_matches_library_semantics_and_owns_its_snapshot() {
    let fixture = Fixture::new();
    let (runtime, _connection) = fixture.runtime();
    let (shared, workspace, original) = project(&runtime, 16);
    let (brushes, filters, document) = {
        let engine = shared.lock().unwrap();
        let crate::engine::view::DocumentSnapshot::Native(document) = engine.doc_snapshot() else {
            panic!("Native source required")
        };
        (
            engine.brush_library.clone(),
            engine.filter_library.clone(),
            document,
        )
    };
    let preset = brushes
        .lock()
        .unwrap()
        .save(None, "Frozen", "Basic", crate::brush::Settings::default())
        .unwrap();
    let commands = vec![
        json!({"op":"paint","layer":original.layers[0].id,"preset":preset.id,"points":[[3,3],[7,7]],"color":[31,73,149,177],"soft":true}),
        json!({"op":"filter.add","layer":original.layers[0].id,"preset":"soft-contrast","weight":0.7}),
    ];
    let expected = filters.lock().unwrap().resolve_commands(&commands).unwrap();
    let expected = brushes.lock().unwrap().resolve_commands(&expected).unwrap();
    let before = document.budget().used();
    let frozen = engine::native::presets::freeze_shared(&shared, &document, &commands, &|| Ok(()))
        .unwrap()
        .unwrap();
    assert_eq!(&**frozen, &expected);
    assert!(document.budget().used() > before);
    brushes.lock().unwrap().delete(&preset.id).unwrap();
    assert_eq!(&**frozen, &expected);
    assert!(
        engine::native::presets::freeze_shared(&shared, &document, &commands, &|| Ok(())).is_err()
    );
    drop(frozen);
    assert_eq!(document.budget().used(), before);
    let canceled =
        engine::native::presets::freeze_shared(&shared, &document, &[commands[1].clone()], &|| {
            Err("Canceled".into())
        });
    assert!(canceled.is_err());
    assert_eq!(document.budget().used(), before);
    assert_eq!(shared.lock().unwrap().history_lengths(), [0, 0]);
    native::finish_workspace(&workspace);
}
