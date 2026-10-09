use peerbrush::{
    engine::{Document, Engine, Scope},
    server::{self, Shared},
    workspace::{
        self,
        lifecycle::{finish_exit, Reviewed},
    },
};
use serde_json::json;
use std::sync::{Arc, Mutex};

fn fixture(depth: u16) -> (Shared, Shared, workspace::Registry) {
    let mut e = Engine::new();
    e.doc = Document::new_depth(8, 6, depth).unwrap();
    let root = Arc::new(Mutex::new(e));
    let registry = workspace::attach(&root);
    let second = workspace::new_project(&root, 8, 6, depth, None).unwrap();
    (root, second, registry)
}
fn reviewed(shared: &Shared, discard: bool) -> Reviewed {
    let e = shared.lock().unwrap();
    Reviewed {
        project: e.project_id.clone(),
        document: e.doc.id.clone(),
        revision: e.doc.revision,
        discard,
    }
}
fn dirty(shared: &Shared, name: &str) {
    let mut e = shared.lock().unwrap();
    let layer = e.doc.layers[0].id.clone();
    e.edit(
        "human",
        &[json!({"op":"layer.update","layer":layer,"name":name})],
        None,
        None,
        name,
    )
    .unwrap();
}
fn unchanged(root: &Shared, second: &Shared, before: &[Vec<u8>; 2]) {
    for (shared, bytes) in [root, second].into_iter().zip(before) {
        let e = shared.lock().unwrap();
        assert!(!e.closed);
        assert_eq!(e.doc.export_png().unwrap(), *bytes);
    }
}

#[test]
fn final_exit_requires_every_dirty_source_and_never_closes_a_subset() {
    for depth in [8, 16] {
        let (root, second, registry) = fixture(depth);
        dirty(&root, "Unsaved one");
        dirty(&second, "Unsaved two");
        let before = [
            root.lock().unwrap().doc.export_png().unwrap(),
            second.lock().unwrap().doc.export_png().unwrap(),
        ];
        assert!(finish_exit(
            &registry,
            &[reviewed(&root, true), reviewed(&second, false)]
        )
        .is_err());
        unchanged(&root, &second, &before);
        assert_eq!(workspace::handles_in(&registry).len(), 2);
        assert_eq!(second.lock().unwrap().doc.layers[0].name, "Unsaved two");
        assert_eq!(root.lock().unwrap().undo.len(), 1);
        finish_exit(&registry, &[reviewed(&root, true), reviewed(&second, true)]).unwrap();
        assert!(root.lock().unwrap().closed && second.lock().unwrap().closed);
        assert_eq!(root.lock().unwrap().doc.bit_depth, depth);
        assert_eq!(root.lock().unwrap().doc.export_png().unwrap(), before[0]);
        assert!(workspace::new_project(&root, 8, 6, depth, None).is_err());
    }
}
#[test]
fn newer_human_work_and_replaced_source_invalidate_discard_reviews() {
    let (root, second, registry) = fixture(16);
    dirty(&root, "Old");
    let old = vec![reviewed(&root, true), reviewed(&second, false)];
    dirty(&root, "New human work");
    assert!(finish_exit(&registry, &old).is_err());
    assert!(!second.lock().unwrap().closed);
    assert_eq!(root.lock().unwrap().doc.layers[0].name, "New human work");
    let old = vec![reviewed(&root, true), reviewed(&second, false)];
    second.lock().unwrap().doc.id = peerbrush::engine::id();
    assert!(finish_exit(&registry, &old).is_err());
    assert!(!root.lock().unwrap().closed);
}
#[test]
fn changed_project_set_missing_or_duplicate_decisions_preserve_all_projects() {
    let (root, second, registry) = fixture(8);
    let rows = vec![reviewed(&root, false), reviewed(&second, false)];
    assert!(finish_exit(&registry, &rows[..1]).is_err());
    assert!(finish_exit(&registry, &[rows[0].clone(), rows[0].clone()]).is_err());
    let third = workspace::new_project(&root, 8, 6, 16, None).unwrap();
    assert!(finish_exit(&registry, &rows).is_err());
    assert!(![&root, &second, &third]
        .iter()
        .any(|e| e.lock().unwrap().closed));
}
#[test]
fn reservations_reject_the_whole_exit_without_closing_clean_or_discarded_projects() {
    let (root, second, registry) = fixture(16);
    dirty(&root, "Reviewed discard");
    second
        .lock()
        .unwrap()
        .reserve(
            "agent",
            "Current work",
            vec![Scope {
                target: None,
                rect: None,
            }],
        )
        .unwrap();
    let rows = vec![reviewed(&root, true), reviewed(&second, false)];
    assert!(finish_exit(&registry, &rows).is_err());
    assert!(!root.lock().unwrap().closed && !second.lock().unwrap().closed);
    second.lock().unwrap().leases.clear();
    finish_exit(&registry, &rows).unwrap();
}
#[test]
fn failed_save_keeps_dirty_state_history_and_both_projects_open() {
    let (root, second, registry) = fixture(16);
    dirty(&root, "Keep unsaved");
    let dir = std::env::temp_dir().join(format!("pb-lifecycle-{}", peerbrush::engine::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let blocked = dir.join("blocked.psd");
    std::fs::create_dir(&blocked).unwrap();
    let source = reviewed(&root, false);
    assert!(server::save_source(&root, &blocked, &source.document).is_err());
    assert!(finish_exit(&registry, &[source, reviewed(&second, false)]).is_err());
    let e = root.lock().unwrap();
    assert!(!e.closed);
    assert_eq!(e.saved_revision, 0);
    assert_eq!(e.undo.len(), 1);
    assert_eq!(e.doc.layers[0].name, "Keep unsaved");
    assert!(!second.lock().unwrap().closed);
}
#[test]
fn successful_save_retains_native_samples_and_current_standard_composite_before_exit() {
    let (root, second, registry) = fixture(16);
    root.lock().unwrap().doc.layers[0]
        .pixels
        .set16(2, 3, [10001, 30003, 50007, 65535]);
    dirty(&root, "Native source");
    let path = std::env::temp_dir().join(format!("pb-lifecycle-{}.psd", peerbrush::engine::id()));
    let source = reviewed(&root, false);
    server::save_source(&root, &path, &source.document).unwrap();
    let reopened = peerbrush::psd::decode(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(reopened.bit_depth, 16);
    assert_eq!(
        reopened.layers[0].pixels.get16(2, 3),
        [10001, 30003, 50007, 65535]
    );
    assert_eq!(
        reopened.export_png().unwrap(),
        root.lock().unwrap().doc.export_png().unwrap()
    );
    finish_exit(&registry, &[source, reviewed(&second, false)]).unwrap();
    let e = root.lock().unwrap();
    assert_eq!(e.saved_revision, e.doc.revision);
}

#[test]
fn reviewed_save_rejects_a_newer_revision_before_capturing_or_writing() {
    let (root, _, _) = fixture(16);
    dirty(&root, "Reviewed work");
    let source = reviewed(&root, false);
    dirty(&root, "New human work");
    let path =
        std::env::temp_dir().join(format!("pb-reviewed-save-{}.psd", peerbrush::engine::id()));
    assert!(server::save_reviewed_source(&root, &path, &source.document, source.revision).is_err());
    assert!(!path.exists());
    let e = root.lock().unwrap();
    assert!(!e.closed);
    assert_eq!(e.saved_revision, 0);
    assert_eq!(e.doc.layers[0].name, "New human work");
    assert_eq!(e.undo.len(), 2);
}

#[test]
fn requesting_native_close_review_requires_human_and_exact_source_without_closing_or_editing() {
    let (root, second, _) = fixture(16);
    dirty(&second, "Review in native workspace");
    let source = reviewed(&second, false);
    let request = json!({"action":"request_close","actor":"human","project_id":source.project,
        "document_id":source.document,"expected_revision":source.revision});
    let mut invalid = request.clone();
    invalid["actor"] = json!("agent");
    assert!(server::dispatch(&root, "projects", &invalid).is_err());
    invalid = request.clone();
    invalid["expected_revision"] = json!(0);
    assert!(server::dispatch(&root, "projects", &invalid).is_err());
    let result = server::dispatch(&root, "projects", &request).unwrap();
    assert_eq!(result["closed"], false);
    assert_eq!(result["native_review"], true);
    let e = second.lock().unwrap();
    assert!(!e.closed);
    assert_eq!(
        e.native_close_review,
        Some((source.document, source.revision))
    );
    assert_eq!(e.doc.layers[0].name, "Review in native workspace");
    assert_eq!(e.doc.revision, 1);
    assert_eq!(e.undo.len(), 1);
    assert!(!root.lock().unwrap().closed);
}

#[test]
fn a_busy_project_keeps_every_exit_review_open_without_waiting_for_its_engine() {
    let (root, second, registry) = fixture(16);
    let rows = vec![reviewed(&root, false), reviewed(&second, false)];
    let source = reviewed(&second, false);
    let _busy = second.lock().unwrap();
    assert!(finish_exit(&registry, &rows).is_err());
    assert!(workspace::close_in(
        &registry,
        &source.project,
        &source.document,
        source.revision,
        false,
        "human"
    )
    .is_err());
    assert!(!root.lock().unwrap().closed);
}
