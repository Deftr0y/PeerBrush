use peerbrush::{
    effects::Effect,
    engine::{Document, Engine, Layer, Mask, MaskStep, Scope},
    raster::Raster,
    server::{self, Shared},
    workspace as w,
};
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};

fn fixture() -> (Shared, Shared) {
    let mut e = Engine::new();
    e.doc = Document::new(12, 10).unwrap();
    let root = Arc::new(Mutex::new(e));
    w::attach(&root);
    let second = w::new_project(&root, 12, 10, 8, None).unwrap();
    (root, second)
}
fn target(shared: &Shared) -> Value {
    let e = shared.lock().unwrap();
    json!({"project_id":e.project_id,"document_id":e.doc.id,"expected_revision":e.doc.revision,"feedback":"request"})
}
fn edit(root: &Shared, shared: &Shared, name: &str) -> Value {
    let mut p = target(shared);
    p["commands"] =
        json!([{"op":"layer.update","layer":shared.lock().unwrap().doc.layers[0].id,"name":name}]);
    server::dispatch(root, "edit", &p).unwrap()
}
fn transfer(
    root: &Shared,
    source: &Shared,
    dest: &Shared,
    move_layers: bool,
) -> Result<Value, String> {
    let from = source.lock().unwrap();
    let to = dest.lock().unwrap();
    let source_id = from.project_id.clone();
    let dest_id = to.project_id.clone();
    let source_doc = from.doc.id.clone();
    let dest_doc = to.doc.id.clone();
    let source_rev = from.doc.revision;
    let dest_rev = to.doc.revision;
    let layers = vec![from.doc.layers[0].id.clone()];
    let target = to.doc.layers[0].id.clone();
    drop(to);
    drop(from);
    w::transfer(
        root,
        &w::Transfer {
            source: &source_id,
            destination: &dest_id,
            source_document: &source_doc,
            destination_document: &dest_doc,
            source_revision: source_rev,
            destination_revision: dest_rev,
            layers: &layers,
            target: &target,
            move_layers,
            actor: "artist",
            source_task: None,
            destination_task: None,
        },
    )
}

#[test]
fn targeted_background_edits_never_select_tabs_or_share_history_selection_or_dirty_state() {
    let (root, second) = fixture();
    let initial = w::active_id(&root);
    edit(&root, &second, "Background work");
    assert_eq!(w::active_id(&root), initial);
    assert_eq!(root.lock().unwrap().doc.revision, 0);
    let mut p = target(&second);
    p["commands"] = json!([{"op":"selection","rect":[1,2,5,7],"kind":"rectangle"}]);
    server::dispatch(&root, "edit", &p).unwrap();
    assert!(root.lock().unwrap().doc.selection.is_none());
    assert_eq!(second.lock().unwrap().undo.len(), 2);
    let mut p = target(&second);
    p["action"] = json!("undo");
    server::dispatch(&root, "history", &p).unwrap();
    assert!(second.lock().unwrap().doc.selection.is_none());
    assert_eq!(second.lock().unwrap().doc.layers[0].name, "Background work");
    assert!(root.lock().unwrap().undo.is_empty());
    assert_eq!(root.lock().unwrap().saved_revision, 0);
}
#[test]
fn ambiguous_missing_closed_and_stale_targets_never_edit_another_project() {
    let (root, second) = fixture();
    let command =
        json!({"commands":[{"op":"layer.add","name":"Wrong project"}],"feedback":"request"});
    assert!(server::dispatch(&root, "edit", &command)
        .unwrap_err()
        .contains("Multiple projects"));
    let mut p = target(&second);
    p["commands"] = command["commands"].clone();
    p["project_id"] = json!("missing");
    assert!(server::dispatch(&root, "edit", &p).is_err());
    p = target(&second);
    p["commands"] = command["commands"].clone();
    p["document_id"] = json!("stale");
    assert!(server::dispatch(&root, "edit", &p).is_err());
    p = target(&second);
    p["commands"] = command["commands"].clone();
    let id = p["project_id"].as_str().unwrap().to_owned();
    let doc = p["document_id"].as_str().unwrap().to_owned();
    w::close(&root, &id, &doc, 0, false, "human").unwrap();
    assert!(server::dispatch(&root, "edit", &p).is_err());
    assert!(second
        .lock()
        .unwrap()
        .edit("human", &[], None, None, "Closed")
        .is_err());
    assert_eq!(root.lock().unwrap().doc.layers.len(), 1);
}
#[test]
fn targeted_mutations_need_the_observed_document_and_revision() {
    let (root, second) = fixture();
    let mut p = target(&second);
    p["commands"] = json!([{"op":"layer.add","name":"Add"}]);
    p.as_object_mut().unwrap().remove("document_id");
    assert!(server::dispatch(&root, "edit", &p).is_err());
    p = target(&second);
    p["commands"] = json!([{"op":"layer.add","name":"Add"}]);
    edit(&root, &second, "Newer");
    assert!(server::dispatch(&root, "edit", &p).is_err());
    assert_eq!(second.lock().unwrap().doc.layers.len(), 1);
}
#[test]
fn closing_dirty_or_reserved_projects_preserves_work_and_last_close_creates_blank_tab() {
    let (root, second) = fixture();
    edit(&root, &second, "Unsaved");
    let p = target(&second);
    let (id, doc) = (
        p["project_id"].as_str().unwrap(),
        p["document_id"].as_str().unwrap(),
    );
    assert!(w::close(&root, id, doc, 1, false, "human").is_err());
    assert_eq!(w::handles(&root).len(), 2);
    second
        .lock()
        .unwrap()
        .reserve(
            "agent",
            "Working",
            vec![Scope {
                target: None,
                rect: None,
            }],
        )
        .unwrap();
    assert!(w::close(&root, id, doc, 1, true, "human").is_err());
    assert_eq!(second.lock().unwrap().doc.layers[0].name, "Unsaved");
    second.lock().unwrap().leases.clear();
    w::close(&root, id, doc, 1, true, "human").unwrap();
    let p = target(&root);
    w::close(
        &root,
        p["project_id"].as_str().unwrap(),
        p["document_id"].as_str().unwrap(),
        0,
        false,
        "human",
    )
    .unwrap();
    assert_eq!(w::handles(&root).len(), 1);
    let fresh = w::active(&root).unwrap();
    assert!(!Arc::ptr_eq(&fresh, &root));
    assert!(!fresh.lock().unwrap().closed);
}
#[test]
fn copy_and_move_are_independently_undoable_and_move_failure_rolls_back_both_engines() {
    let (root, second) = fixture();
    let before = serde_json::to_value(&second.lock().unwrap().doc).unwrap();
    root.lock()
        .unwrap()
        .reserve(
            "owner",
            "Source reserved",
            vec![Scope {
                target: None,
                rect: None,
            }],
        )
        .unwrap();
    assert!(transfer(&root, &root, &second, true).is_err());
    assert_eq!(
        serde_json::to_value(&second.lock().unwrap().doc).unwrap(),
        before
    );
    assert!(second.lock().unwrap().undo.is_empty());
    assert!(second.lock().unwrap().ai_change.is_none());
    transfer(&root, &root, &second, false).unwrap();
    assert!(root.lock().unwrap().undo.is_empty());
    assert_eq!(second.lock().unwrap().undo.len(), 1);
    second.lock().unwrap().undo("human").unwrap();
    root.lock().unwrap().leases.clear();
    transfer(&root, &root, &second, true).unwrap();
    assert!(root.lock().unwrap().doc.layers.is_empty());
    assert_eq!(root.lock().unwrap().undo.len(), 1);
    root.lock().unwrap().undo("human").unwrap();
    second.lock().unwrap().undo("human").unwrap();
    assert_eq!(root.lock().unwrap().doc.layers.len(), 1);
    assert_eq!(second.lock().unwrap().doc.layers.len(), 1);
}
#[test]
fn transfer_preview_and_paste_preserve_native_tree_masks_effects_and_standard_psd_composite() {
    let (root, second) = fixture();
    {
        let mut e = root.lock().unwrap();
        e.doc = Document::new_depth(12, 10, 16).unwrap();
        let mut folder = Layer::new("Native folder", "group", 12, 10);
        folder.pixels.promote16();
        let mut child = e.doc.layers.remove(0);
        child.parent = Some(folder.id.clone());
        child.name = "Native ink".into();
        child.pixels.set16(2, 3, [10001, 30003, 50007, 65535]);
        let mut pixels = Raster::new_depth(12, 10, 16);
        pixels.set16(2, 3, [40001, 40001, 40001, 65535]);
        child.mask = Some(Mask {
            enabled: true,
            cache_key: peerbrush::engine::id(),
            steps: vec![MaskStep {
                weight: 0.75,
                id: peerbrush::engine::id(),
                kind: "paint".into(),
                enabled: true,
                value: 255.,
                pixels,
                settings: json!({}),
            }],
        });
        child.effects.push(Effect {
            weight: 0.4,
            id: peerbrush::engine::id(),
            kind: "invert".into(),
            enabled: true,
            settings: json!({}),
        });
        e.doc.layers = vec![folder, child];
    }
    let p = target(&second);
    let s = target(&root);
    let layers = vec![root.lock().unwrap().doc.layers[0].id.clone()];
    let target = second.lock().unwrap().doc.layers[0].id.clone();
    let request = w::Transfer {
        source: s["project_id"].as_str().unwrap(),
        destination: p["project_id"].as_str().unwrap(),
        source_document: s["document_id"].as_str().unwrap(),
        destination_document: p["document_id"].as_str().unwrap(),
        source_revision: 0,
        destination_revision: 0,
        layers: &layers,
        target: &target,
        move_layers: false,
        actor: "human",
        source_task: None,
        destination_task: None,
    };
    let preview = w::transfer_preview(&root, &request).unwrap();
    assert_eq!(preview.bit_depth, 16);
    assert!(second.lock().unwrap().undo.is_empty());
    w::transfer(&root, &request).unwrap();
    let e = second.lock().unwrap();
    let ink = e
        .doc
        .layers
        .iter()
        .find(|l| l.name == "Native ink")
        .unwrap();
    assert_eq!(ink.pixels.get16(2, 3), [10001, 30003, 50007, 65535]);
    assert_eq!(
        ink.mask.as_ref().unwrap().steps[0].pixels.get16(2, 3)[0],
        40001
    );
    assert_eq!(ink.effects[0].weight, 0.4);
    assert_eq!(e.doc.export_png().unwrap(), preview.export_png().unwrap());
    let encoded = peerbrush::psd::encode(&e.doc).unwrap();
    let reopened = peerbrush::psd::decode(&encoded).unwrap();
    assert_eq!(reopened.export_png().unwrap(), e.doc.export_png().unwrap());
    assert_eq!(reopened.bit_depth, 16);
}
#[test]
fn duplicate_opens_have_distinct_runtime_sources_and_keep_independent_save_conflict_guards() {
    let (root, _) = fixture();
    let dir = std::env::temp_dir().join(format!("pb-projects-{}", peerbrush::engine::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("shared.psd");
    server::save(&root, &path).unwrap();
    let a = server::dispatch(&root, "projects", &json!({"action":"open","path":path})).unwrap();
    let b = server::dispatch(&root, "projects", &json!({"action":"open","path":path})).unwrap();
    assert_ne!(a["project_id"], b["project_id"]);
    assert_ne!(a["document"]["id"], b["document"]["id"]);
    let a = w::get(&root, a["project_id"].as_str().unwrap()).unwrap();
    let b = w::get(&root, b["project_id"].as_str().unwrap()).unwrap();
    edit(&root, &a, "Saved first");
    server::save(&a, &path).unwrap();
    edit(&root, &b, "Other edits");
    assert!(server::save(&b, &path).unwrap_err().contains("outside"));
    assert_eq!(b.lock().unwrap().saved_revision, 0);
    assert_eq!(b.lock().unwrap().doc.layers[0].name, "Other edits");
    let closed = w::new_project(&root, 12, 10, 8, None).unwrap();
    let p = target(&closed);
    w::close(
        &root,
        p["project_id"].as_str().unwrap(),
        p["document_id"].as_str().unwrap(),
        0,
        false,
        "human",
    )
    .unwrap();
    assert!(server::save(&closed, &dir.join("closed.psd")).is_err());
    assert!(!dir.join("closed.psd").exists());
}
#[test]
fn project_selection_and_targeted_http_remain_responsive_while_root_engine_is_busy() {
    let (root, second) = fixture();
    let dir = std::env::temp_dir().join(format!("pb-project-http-{}", peerbrush::engine::id()));
    let connection = server::start(root.clone(), dir.clone()).unwrap();
    let registry = w::attach(&root);
    let p = target(&second);
    let root_guard = root.lock().unwrap();
    w::select_in(&registry, p["project_id"].as_str().unwrap()).unwrap();
    let response: Value = ureq::post(&format!("http://127.0.0.1:{}/rpc", connection.port))
        .set("Authorization", &format!("Bearer {}", connection.token))
        .timeout(std::time::Duration::from_secs(2))
        .send_json(
            json!({"method":"observe","params":{"project_id":p["project_id"],"image":false}}),
        )
        .unwrap()
        .into_json()
        .unwrap();
    assert_eq!(response["result"]["project_id"], p["project_id"]);
    drop(root_guard);
    drop(connection);
}
#[test]
fn recovery_indexes_every_dirty_project_and_restore_paths_stay_local() {
    let (root, second) = fixture();
    edit(&root, &root, "One");
    edit(&root, &second, "Two");
    let dir = std::env::temp_dir().join(format!("pb-project-recovery-{}", peerbrush::engine::id()));
    std::fs::create_dir_all(&dir).unwrap();
    server::checkpoint_projects(&w::attach(&root), &dir, &mut Default::default());
    let entries = server::recovery_projects(&dir);
    assert_eq!(entries.len(), 2);
    for (_, path) in entries {
        assert_eq!(path.parent(), Some(dir.as_path()));
        let doc = peerbrush::psd::decode(&std::fs::read(path).unwrap()).unwrap();
        assert!(matches!(doc.layers[0].name.as_str(), "One" | "Two"));
    }
    std::fs::write(
        dir.join("recoveries.json"),
        br#"{"projects":[{"project_id":"../../escape","file":"outside.psd"}]}"#,
    )
    .unwrap();
    assert!(server::recovery_projects(&dir).is_empty());
}

#[test]
fn reopening_same_psd_rejects_old_project_source_even_at_identical_saved_revision() {
    let (root, second) = fixture();
    let dir = std::env::temp_dir().join(format!("pb-project-reopen-{}", peerbrush::engine::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("source.psd");
    server::save(&second, &path).unwrap();
    let mut old = target(&second);
    old["commands"] = json!([{"op":"layer.add","name":"Stale"}]);
    server::open(&second, &path).unwrap();
    assert_eq!(second.lock().unwrap().doc.revision, 0);
    assert!(server::dispatch(&root, "edit", &old).is_err());
    assert!(server::save_source(
        &second,
        &dir.join("stale-save.psd"),
        old["document_id"].as_str().unwrap()
    )
    .is_err());
    assert!(!dir.join("stale-save.psd").exists());
    assert!(server::export_source(
        &second,
        &dir.join("stale.png"),
        old["document_id"].as_str().unwrap()
    )
    .is_err());
    assert!(!dir.join("stale.png").exists());
}
#[test]
fn protected_sources_require_explicit_compatible_copy_before_tree_transfer() {
    let (root, second) = fixture();
    root.lock().unwrap().doc.read_only = true;
    assert!(transfer(&root, &root, &second, false)
        .unwrap_err()
        .contains("compatible copy"));
    assert!(second.lock().unwrap().undo.is_empty());
    assert!(root.lock().unwrap().doc.read_only);
}
