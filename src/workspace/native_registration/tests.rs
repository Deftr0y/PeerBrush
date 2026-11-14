use super::*;
use crate::{engine::native::worker::tests::Fixture, render::native_preview::Request};

fn workspace(runtime: &Arc<SourceRuntime>) -> (Shared, Registry) {
    let root = Arc::new(Mutex::new(Engine::new()));
    let workspace = attach(&root);
    workspace.lock().unwrap().source_runtime = Some(runtime.clone());
    (root, workspace)
}
fn current(root: &Shared) -> Value {
    let engine = root.lock().unwrap();
    serde_json::to_value(&engine.resident_state().unwrap().document).unwrap()
}
fn prepared(workspace: &Registry, runtime: &Arc<SourceRuntime>) -> Prepared {
    let document =
        NativeDocument::new_blank(19, 13, 16, &runtime.resident(), &runtime.disk(), &|| Ok(()))
            .unwrap();
    Prepared::new(workspace, runtime, document, &Control::default()).unwrap()
}

#[test]
fn native_blank_tabs_preserve_zero_source_absence_depth_and_existing_work() {
    for depth in [8, 16] {
        let fixture = Fixture::new();
        let (runtime, _connection) = fixture.runtime();
        let (root, workspace) = workspace(&runtime);
        let initiating = root.lock().unwrap().project_id.clone();
        let before = current(&root);
        let control = Control::default();
        let shared = new_blank(&workspace, 19, 13, depth, &control, Some(&initiating)).unwrap();
        assert!(new_blank(&workspace, 19, 13, depth, &control, Some(&initiating)).is_err());
        let scene = {
            let engine = shared.lock().unwrap();
            assert_eq!(active_id_in(&workspace), engine.project_id);
            assert_eq!(engine.history_lengths(), [0, 0]);
            assert_eq!(engine.saved_revision, 0);
            assert!(Arc::ptr_eq(
                &engine.brush_library,
                &root.lock().unwrap().brush_library
            ));
            let document = engine.native_document().unwrap();
            assert_eq!(
                (
                    document.metadata().width(),
                    document.metadata().height(),
                    document.metadata().depth()
                ),
                (19, 13, depth)
            );
            assert_eq!(document.metadata().revision(), 0);
            let layer = document.metadata().layer(0).unwrap();
            assert!(document
                .source(layer.pixels().slot())
                .unwrap()
                .is_known_zero());
            Arc::new(engine.capture_scene(None).unwrap().into_native().unwrap())
        };
        let read = runtime.begin_read(&scene).unwrap();
        let request = Request::capture(scene, read, None, 32, None, false).unwrap();
        let frame = request.render(&mut || Ok(())).unwrap();
        assert!(frame
            .copy_rgba8(&mut || Ok(()))
            .unwrap()
            .iter()
            .all(|byte| *byte == 0));
        assert_eq!(runtime.disk().used().unwrap(), 0);
        assert!(!fixture.path().join("state/native-sources-v1").exists());
        assert_eq!(current(&root), before);
        assert_eq!(root.lock().unwrap().history_lengths(), [0, 0]);
        assert_eq!(entries_in(&workspace).len(), 2);
    }
}

#[test]
fn native_registration_final_fence_refuses_late_changes_without_publishing_or_dropping_the_owner() {
    for failure in ["cancel", "service", "exit", "busy", "duplicate", "capacity"] {
        let fixture = Fixture::new();
        let (runtime, _connection) = fixture.runtime();
        let (root, workspace) = workspace(&runtime);
        let mut prepared = prepared(&workspace, &runtime);
        let retained = Arc::downgrade(&prepared.shared);
        let control = Control::default();
        let scene = Arc::new(
            prepared
                .shared
                .lock()
                .unwrap()
                .capture_scene(None)
                .unwrap()
                .into_native()
                .unwrap(),
        );
        let session = runtime.begin_read(&scene).unwrap();
        match failure {
            "cancel" => control.cancel(),
            "service" => workspace.lock().unwrap().source_runtime = None,
            "exit" => workspace.lock().unwrap().exiting = true,
            "duplicate" => {
                prepared.entry.as_mut().unwrap().id = root.lock().unwrap().project_id.clone()
            }
            "capacity" => {
                for _ in 1..MAX_PROJECTS {
                    register_in(
                        &workspace,
                        Engine::from_document(Document::new(1, 1).unwrap()),
                        None,
                    )
                    .unwrap();
                }
            }
            "busy" => (),
            _ => unreachable!(),
        }
        let before = entries_in(&workspace).len();
        let held = (failure == "busy").then(|| workspace.lock().unwrap());
        assert!(
            session
                .with_live_fence(|| prepared.publish(&workspace, &control, None))
                .unwrap()
                .is_err(),
            "{failure}"
        );
        drop(held);
        assert_eq!(entries_in(&workspace).len(), before, "{failure}");
        assert!(prepared.entry.is_some());
        assert!(retained.upgrade().is_some());
        assert!(!fixture.path().join("state/native-sources-v1").exists());
        assert_eq!(root.lock().unwrap().history_lengths(), [0, 0]);
        // Restore the real configured service outside the publication fence.
        workspace.lock().unwrap().source_runtime = Some(runtime.clone());
    }
}

#[test]
fn native_registration_respects_conditional_activation_and_missing_service() {
    let fixture = Fixture::new();
    let (runtime, _connection) = fixture.runtime();
    let (root, workspace) = workspace(&runtime);
    let initial = active_id_in(&workspace);
    let other = register_in(
        &workspace,
        Engine::from_document(Document::new(1, 1).unwrap()),
        Some(&initial),
    )
    .unwrap();
    let selected = other.lock().unwrap().project_id.clone();
    let new = new_blank(&workspace, 19, 13, 16, &Control::default(), Some(&initial)).unwrap();
    assert_eq!(active_id_in(&workspace), selected);
    assert_ne!(new.lock().unwrap().project_id, initial);
    assert_eq!(entries_in(&workspace).len(), 3);
    workspace.lock().unwrap().source_runtime = None;
    assert!(new_blank(&workspace, 19, 13, 16, &Control::default(), Some(&initial)).is_err());
    assert_eq!(entries_in(&workspace).len(), 3);
    assert!(workspace.lock().unwrap().source_runtime.is_none());
    assert!(root.lock().unwrap().native_document().is_none());
}

#[test]
fn native_project_new_admits_the_reply_before_registering_an_independent_tab() {
    for (depth, starts_native) in [(8, false), (16, false), (8, true), (16, true)] {
        let fixture = Fixture::new();
        let (runtime, _connection) = fixture.runtime();
        let (root, workspace) = workspace(&runtime);
        let initial = active_id_in(&workspace);
        let before = current(&root);
        let source = if starts_native {
            new_blank(
                &workspace,
                19,
                13,
                depth,
                &Control::default(),
                Some(&initial),
            )
            .unwrap()
        } else {
            root.clone()
        };
        let tabs = entries_in(&workspace).len();
        let (source_project, source_key) = {
            let engine = source.lock().unwrap();
            (
                engine.project_id.clone(),
                engine.native_document().map(NativeDocument::key),
            )
        };
        let request =
            json!({"action":"new","actor":"human","width":29,"height":17,"bit_depth":depth});
        assert!(crate::server::dispatch(&root, "projects", &request).is_err());
        assert_eq!(entries_in(&workspace).len(), tabs);
        assert_eq!(active_id_in(&workspace), source_project);

        let reply = crate::server::dispatch_owned(&root, "projects", &request).unwrap();
        let created = reply.value()["project_id"].as_str().unwrap();
        assert_ne!(created, source_project);
        assert_eq!(active_id_in(&workspace), created);
        let tab = get_in(&workspace, created).unwrap();
        let engine = tab.lock().unwrap();
        let doc = engine.native_document().unwrap();
        let metadata = doc.metadata();
        assert_eq!(
            (metadata.width(), metadata.height(), metadata.depth()),
            (29, 17, depth)
        );
        assert_eq!(engine.history_lengths(), [0, 0]);
        assert!(doc
            .source(metadata.layer(0).unwrap().pixels().slot())
            .unwrap()
            .is_known_zero());
        drop(engine);
        assert_eq!(
            source
                .lock()
                .unwrap()
                .native_document()
                .map(NativeDocument::key),
            source_key
        );
        assert_eq!(current(&root), before);
        assert_eq!(entries_in(&workspace).len(), tabs + 1);
        assert_eq!(runtime.disk().used().unwrap(), 0);
    }
}

#[test]
fn resident_document_new_adopts_native_authority_only_with_discard_and_an_owned_reply() {
    let fixture = Fixture::new();
    let (runtime, _connection) = fixture.runtime();
    let (root, workspace) = workspace(&runtime);
    let (project, document, revision, brushes) = {
        let mut engine = root.lock().unwrap();
        let layer = engine.resident_state().unwrap().document.layers[0]
            .id
            .clone();
        engine
            .edit(
                "human",
                &[json!({"op":"layer.update","layer":layer,"name":"Unsaved layer"})],
                None,
                None,
                "Rename layer",
            )
            .unwrap();
        let document = &engine.resident_state().unwrap().document;
        (
            engine.project_id.clone(),
            document.id.clone(),
            document.revision,
            engine.brush_library.clone(),
        )
    };
    let before = current(&root);
    let mut request = json!({
        "action":"new","actor":"human","project_id":project,"document_id":document,
        "expected_revision":revision,"width":31,"height":23,"bit_depth":16
    });
    assert!(crate::server::dispatch_owned(&root, "document", &request).is_err());
    assert_eq!(current(&root), before);
    request["discard"] = json!(true);
    assert!(crate::server::dispatch(&root, "document", &request).is_err());
    assert_eq!(current(&root), before);

    let reply = crate::server::dispatch_owned(&root, "document", &request).unwrap();
    assert_eq!(reply.value()["project_id"], project.as_str());
    assert_eq!(reply.value()["document"]["bit_depth"], 16);
    assert_eq!(reply.value()["dirty"], false);
    let engine = root.lock().unwrap();
    let native = engine.native_document().unwrap();
    assert_ne!(native.metadata().id(), document);
    assert_eq!(native.metadata().id(), reply.value()["document"]["id"]);
    assert_eq!(engine.history_lengths(), [0, 0]);
    assert!(Arc::ptr_eq(&engine.brush_library, &brushes));
    drop(engine);
    assert_eq!(entries_in(&workspace).len(), 1);
    assert_eq!(active_id_in(&workspace), project);
    assert_eq!(runtime.disk().used().unwrap(), 0);
}

#[test]
fn native_document_new_preserves_the_tab_and_requires_a_current_unreserved_source() {
    let fixture = Fixture::new();
    let (runtime, _connection) = fixture.runtime();
    let (root, workspace) = workspace(&runtime);
    let initial = active_id_in(&workspace);
    let source = new_blank(&workspace, 19, 13, 8, &Control::default(), Some(&initial)).unwrap();
    let (project, document, key, brush_library, reservation) = {
        let mut engine = source.lock().unwrap();
        let reservation = engine
            .reserve(
                "reviewer",
                "Checking visibility",
                vec![crate::engine::Scope {
                    target: Some("@visibility:reviewed-layer".into()),
                    rect: None,
                }],
            )
            .unwrap();
        let native = engine.native_document().unwrap();
        (
            engine.project_id.clone(),
            native.metadata().id().to_owned(),
            native.key(),
            engine.brush_library.clone(),
            reservation.id,
        )
    };
    let request = json!({
        "action":"new", "actor":"human", "project_id":project, "document_id":document,
        "expected_revision":0, "width":29, "height":17, "bit_depth":16
    });
    let refused = crate::server::dispatch_owned(&root, "document", &request)
        .err()
        .unwrap();
    assert!(refused.contains("Reserved by reviewer"));
    assert_eq!(source.lock().unwrap().native_document().unwrap().key(), key);
    source.lock().unwrap().finish_task(&reservation, "released");

    assert!(crate::server::dispatch(&root, "document", &request).is_err());
    assert_eq!(source.lock().unwrap().native_document().unwrap().key(), key);
    let operation = crate::server::FileOperation::begin(&source).unwrap();
    assert!(crate::server::dispatch_owned(&root, "document", &request).is_err());
    assert_eq!(source.lock().unwrap().native_document().unwrap().key(), key);
    drop(operation);

    let reply = crate::server::dispatch_owned(&root, "document", &request).unwrap();
    assert_eq!(reply.value()["project_id"], project.as_str());
    assert_eq!(reply.value()["document"]["bit_depth"], 16);
    assert_eq!(reply.value()["document"]["width"], 29);
    assert_eq!(reply.value()["document"]["height"], 17);
    assert_eq!(reply.value()["dirty"], false);
    {
        let engine = source.lock().unwrap();
        let native = engine.native_document().unwrap();
        assert_ne!(native.key(), key);
        assert_eq!(reply.value()["document"]["id"], native.metadata().id());
        assert_eq!(engine.history_lengths(), [0, 0]);
        assert!(engine.leases.is_empty() && engine.recent_tasks.is_empty());
        assert!(Arc::ptr_eq(&engine.brush_library, &brush_library));
    }
    assert_eq!(entries_in(&workspace).len(), 2);
    assert_eq!(active_id_in(&workspace), project);
    assert!(Arc::ptr_eq(&get_in(&workspace, &project).unwrap(), &source));
    assert!(crate::server::dispatch_owned(&root, "document", &request).is_err());
    assert_eq!(runtime.disk().used().unwrap(), 0);
}
