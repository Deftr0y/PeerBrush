use peerbrush::{
    engine::{Document, Engine, Scope},
    psd, raster, server,
};
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};
fn fixture() -> (server::Shared, String) {
    let mut e = Engine::new();
    e.doc = Document::new_depth(32, 24, 16).unwrap();
    e.doc.layers[0]
        .pixels
        .set16(8, 9, [12345, 23457, 34569, 65535]);
    let layer = e.doc.layers[0].id.clone();
    (Arc::new(Mutex::new(e)), layer)
}
fn request(shared: &server::Shared, commands: Vec<Value>) -> Value {
    let e = shared.lock().unwrap();
    json!({"action":"create","actor":"artist-agent","document_id":e.doc.id,"expected_revision":e.doc.revision,"label":"Proposed retouch","commands":commands,"feedback":"request"})
}
fn create(shared: &server::Shared, commands: Vec<Value>) -> String {
    let result = server::dispatch(shared, "proposal", &request(shared, commands)).unwrap();
    result["proposal"]["id"].as_str().unwrap().into()
}
fn accept(shared: &server::Shared, id: &str) -> Result<Value, String> {
    let e = shared.lock().unwrap();
    let p = json!({"action":"accept","actor":"human","proposal":id,"document_id":e.doc.id,"expected_revision":e.doc.revision,"feedback":"request"});
    drop(e);
    server::dispatch(shared, "proposal", &p)
}
#[test]
fn frozen_external_pixels_preview_without_history_and_accept_as_one_native_edit() {
    let (s, layer) = fixture();
    let before = s.lock().unwrap().doc.export_png().unwrap();
    let path = std::env::temp_dir().join(format!("pb-proposal-{}.png", uuid::Uuid::new_v4()));
    let words = [60001, 12347, 34569, 50123];
    std::fs::write(&path, raster::png16(1, 1, &words).unwrap()).unwrap();
    let id = create(
        &s,
        vec![
            json!({"op":"image.place","path":path,"layer":layer,"new_layer":false,"mode":"replace","rect":[3,4,4,5]}),
            json!({"op":"layer.update","layer":layer,"name":"Reviewed retouch"}),
        ],
    );
    let draft = s.lock().unwrap().proposal_document(&id).unwrap();
    let expected = draft.export_png().unwrap();
    assert_eq!(draft.layers[0].pixels.get16(3, 4), words);
    {
        let e = s.lock().unwrap();
        assert!(e.doc.export_png().unwrap() == before);
        assert_eq!(e.doc.revision, 0);
        assert!(e.undo.is_empty());
        assert_eq!(e.saved_revision, 0);
        assert!(e.ai_change.is_none());
    }
    // Acceptance must use the reviewed source, not re-read a changed/deleted external asset.
    std::fs::write(&path, raster::png16(1, 1, &[1, 2, 3, 65535]).unwrap()).unwrap();
    std::fs::remove_file(&path).unwrap();
    let preview = server::dispatch(
        &s,
        "proposal",
        &json!({"action":"preview","proposal":id,"max_edge":32}),
    )
    .unwrap();
    assert_eq!(preview["images"][0]["document_rect"], json!([0, 0, 32, 24]));
    assert_eq!(preview["images"][0]["preview_only"], true);
    assert!(server::dispatch(&s,"proposal",&json!({"action":"accept","actor":"artist-agent","proposal":id,"document_id":draft.id,"expected_revision":0})).unwrap_err().contains("human"));
    let result = accept(&s, &id).unwrap();
    assert_eq!(result["applied"], 2);
    assert_eq!(result["accepted_by"], "human");
    let mut e = s.lock().unwrap();
    assert_eq!(e.undo.len(), 1);
    assert_eq!(e.doc.revision, 1);
    assert!(e.doc.export_png().unwrap() == expected);
    assert_eq!(e.doc.layers[0].pixels.get16(3, 4), words);
    assert_eq!(e.ai_change.as_ref().unwrap().actor, "artist-agent");
    let saved = psd::decode(&psd::encode(&e.doc).unwrap()).unwrap();
    assert!(saved.export_png().unwrap() == expected);
    e.undo("human").unwrap();
    assert!(e.doc.export_png().unwrap() == before);
    e.redo("human").unwrap();
    assert!(e.doc.export_png().unwrap() == expected);
}
#[test]
fn human_edit_makes_a_proposal_stale_and_releases_its_native_draft() {
    let (s, layer) = fixture();
    let id = create(
        &s,
        vec![json!({"op":"layer.update","layer":layer,"opacity":0.3})],
    );
    let after = {
        let mut e = s.lock().unwrap();
        e.edit(
            "human",
            &[json!({"op":"layer.update","layer":layer,"name":"Human name"})],
            Some(0),
            None,
            "Rename",
        )
        .unwrap();
        e.doc.export_png().unwrap()
    };
    assert!(accept(&s, &id).unwrap_err().contains("changed"));
    let mut e = s.lock().unwrap();
    let state = e.proposals_state();
    assert_eq!(state[0]["status"], "stale");
    assert_eq!(state[0]["operations"], json!(["layer.update"]));
    assert!(e.proposal_document(&id).is_err());
    assert_eq!(e.undo.len(), 1);
    assert_eq!(e.doc.layers[0].name, "Human name");
    assert!(e.doc.export_png().unwrap() == after);
}
#[test]
fn rejected_proposals_and_failed_batches_preserve_sources_and_saved_state() {
    let (s, layer) = fixture();
    let before = serde_json::to_value(&s.lock().unwrap().doc).unwrap();
    assert!(server::dispatch(
        &s,
        "proposal",
        &request(
            &s,
            vec![
                json!({"op":"layer.update","layer":layer,"name":"Never commit"}),
                json!({"op":"unsupported.operation","layer":layer})
            ]
        )
    )
    .is_err());
    let id = create(
        &s,
        vec![json!({"op":"layer.update","layer":layer,"opacity":0.5})],
    );
    assert!(server::dispatch(
        &s,
        "proposal",
        &json!({"action":"reject","actor":"other-agent","proposal":id})
    )
    .is_err());
    server::dispatch(
        &s,
        "proposal",
        &json!({"action":"reject","actor":"artist-agent","proposal":id}),
    )
    .unwrap();
    assert!(accept(&s, &id).is_err());
    let mut e = s.lock().unwrap();
    assert_eq!(serde_json::to_value(&e.doc).unwrap(), before);
    assert!(e.undo.is_empty());
    assert_eq!(e.saved_revision, e.doc.revision);
    assert_eq!(e.proposals_state()[0]["status"], "rejected");
}
#[test]
fn reservations_locks_and_changed_project_ids_guard_preparation_and_acceptance() {
    let (s, layer) = fixture();
    let mut p = request(
        &s,
        vec![json!({"op":"layer.update","layer":layer,"name":"Draft"})],
    );
    p["document_id"] = json!("another-project");
    assert!(server::dispatch(&s, "proposal", &p).is_err());
    let reservation = s
        .lock()
        .unwrap()
        .reserve("other-agent", "Protected", vec![Scope::layer(&layer)])
        .unwrap();
    assert!(server::dispatch(
        &s,
        "proposal",
        &request(
            &s,
            vec![json!({"op":"paint.fill","layer":layer,"rect":[0,0,1,1],"color":[255,0,0,255]})]
        )
    )
    .is_err());
    server::dispatch(
        &s,
        "task",
        &json!({"action":"end","actor":"other-agent","task":reservation.id}),
    )
    .unwrap();
    s.lock().unwrap().doc.layers[0].locked = true;
    assert!(server::dispatch(
        &s,
        "proposal",
        &request(
            &s,
            vec![json!({"op":"paint.fill","layer":layer,"rect":[0,0,1,1],"color":[255,0,0,255]})]
        )
    )
    .is_err());
    s.lock().unwrap().doc.layers[0].locked = false;
    let id = create(
        &s,
        vec![json!({"op":"layer.update","layer":layer,"name":"Draft"})],
    );
    s.lock()
        .unwrap()
        .reserve(
            "other-agent",
            "Reserved during review",
            vec![Scope::layer(&layer)],
        )
        .unwrap();
    assert!(accept(&s, &id).unwrap_err().contains("Reserved"));
    let mut e = s.lock().unwrap();
    e.take_over_tasks();
    e.replace(Document::new_depth(16, 16, 16).unwrap(), None)
        .unwrap();
    assert!(e.proposal_document(&id).is_err());
    assert!(e.undo.is_empty());
}
#[test]
fn takeover_end_and_expiry_keep_committed_human_work_and_expose_recovery() {
    for reason in ["taken_over", "ended", "expired"] {
        let (s, layer) = fixture();
        let task = s
            .lock()
            .unwrap()
            .reserve("artist-agent", "Retouch the portrait", vec![])
            .unwrap()
            .id;
        {
            let mut e = s.lock().unwrap();
            e.edit(
                "artist-agent",
                &[json!({"op":"layer.update","layer":layer,"name":"Agent name"})],
                Some(0),
                Some(&task),
                "Retouch",
            )
            .unwrap();
            e.edit(
                "human",
                &[json!({"op":"layer.update","layer":layer,"opacity":0.6})],
                Some(1),
                None,
                "Human opacity",
            )
            .unwrap();
        }
        let mut p = request(
            &s,
            vec![json!({"op":"layer.update","layer":layer,"name":"Unaccepted name"})],
        );
        p["task"] = json!(task);
        let id = server::dispatch(&s, "proposal", &p).unwrap()["proposal"]["id"]
            .as_str()
            .unwrap()
            .to_owned();
        match reason {
            "taken_over" => s.lock().unwrap().take_over_tasks(),
            "ended" => {
                server::dispatch(
                    &s,
                    "task",
                    &json!({"action":"end","actor":"artist-agent","task":task}),
                )
                .unwrap();
            }
            _ => {
                let mut e = s.lock().unwrap();
                e.leases[0].expires = 0;
                e.expire();
            }
        }
        assert!(accept(&s, &id).is_err());
        let mut e = s.lock().unwrap();
        assert_eq!(e.proposals_state()[0]["status"], "interrupted");
        assert_eq!(e.task_recovery()["recent"][0]["status"], reason);
        assert!(e.leases.is_empty());
        assert_eq!(e.undo.len(), 2);
        assert_eq!(e.doc.layers[0].name, "Agent name");
        assert_eq!(e.doc.layers[0].opacity, 0.6);
        assert!(e
            .edit(
                "artist-agent",
                &[json!({"op":"layer.update","layer":layer,"name":"Late name"})],
                Some(2),
                Some(&task),
                "Late write"
            )
            .is_err());
        e.undo_task("human", "artist-agent", &task).unwrap();
        assert_eq!(e.doc.layers[0].name, "Paint 1");
        assert_eq!(e.doc.layers[0].opacity, 0.6);
    }
}
#[test]
fn pending_drafts_are_bounded_and_original_save_metadata_is_kept_on_acceptance() {
    let (s, layer) = fixture();
    let mut ids = vec![];
    for n in 0..4 {
        ids.push(create(
            &s,
            vec![json!({"op":"layer.update","layer":layer,"name":format!("Draft {n}")})],
        ));
    }
    assert!(server::dispatch(
        &s,
        "proposal",
        &request(
            &s,
            vec![json!({"op":"layer.update","layer":layer,"opacity":0.4})]
        )
    )
    .unwrap_err()
    .contains("Four proposals"));
    s.lock().unwrap().doc.name = "Human Save As.psd".into();
    accept(&s, &ids[0]).unwrap();
    let mut e = s.lock().unwrap();
    assert_eq!(e.doc.name, "Human Save As.psd");
    assert_eq!(e.proposals_state()[0]["status"], "accepted");
    assert_eq!(e.proposals_state()[1]["status"], "stale");
    assert_eq!(e.undo.len(), 1);
}
