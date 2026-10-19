use super::*;

#[test]
fn update_restart_is_blocked_during_unlocked_save_and_export_preparation() {
    use crate::workspace::lifecycle::{finish_update_exit, update_readiness};
    let directory = std::env::temp_dir().join(format!("pb-update-save-{}", engine::id()));
    fs::create_dir(&directory).unwrap();
    let mut engine = Engine::new();
    engine.doc = crate::engine::Document::new(8, 8).unwrap();
    let root = Arc::new(Mutex::new(engine));
    let workspace = crate::workspace::attach(&root);
    let reviewed = update_readiness(&workspace).unwrap();
    save_prepared(&root, &directory.join("saved.psd"), &json!({}), || {
        assert!(update_readiness(&workspace).is_err());
        assert!(finish_update_exit(&workspace, &reviewed).is_err());
        assert!(!root.lock().unwrap().closed);
    })
    .unwrap();
    let reviewed = update_readiness(&workspace).unwrap();
    export_derived_guarded(
        &root,
        &directory.join("export.png"),
        &json!({}),
        &crate::production_export::Control::default(),
        |doc| {
            assert!(finish_update_exit(&workspace, &reviewed).is_err());
            doc.export_png()
        },
    )
    .unwrap();
    assert!(update_readiness(&workspace).is_ok());
    assert!(!root.lock().unwrap().closed);
    fs::remove_dir_all(directory).unwrap();
}
#[test]
fn publication_readback_rejects_different_bytes_without_claiming_success() {
    let path = std::env::temp_dir().join(format!("pb-publication-check-{}", engine::id()));
    fs::write(&path, b"external content").unwrap();
    assert!(crate::file_safety::published_version(&path, b"intended content").is_err());
    assert_eq!(fs::read(&path).unwrap(), b"external content");
    fs::remove_file(path).unwrap();
}
#[test]
fn file_fingerprint_checks_are_cancellable_between_bounded_reads() {
    let path = std::env::temp_dir().join(format!("pb-cancel-fingerprint-{}", engine::id()));
    fs::write(&path, vec![42; 1024 * 1024]).unwrap();
    let checks = std::cell::Cell::new(0);
    let result = crate::file_safety::version_with_check(&path, || {
        let n = checks.get() + 1;
        checks.set(n);
        if n == 3 {
            Err("Canceled".into())
        } else {
            Ok(())
        }
    });
    assert_eq!(result.unwrap_err(), "Canceled");
    assert_eq!(checks.get(), 3);
    fs::remove_file(path).unwrap();
}

#[test]
fn pending_save_rejects_changed_document_path_and_destination_at_publication() {
    let dir = std::env::temp_dir().join(format!("pb-pending-save-{}", engine::id()));
    fs::create_dir(&dir).unwrap();
    for scenario in 0..4 {
        let path = dir.join(format!("source-{scenario}.psd"));
        let mut e = Engine::new();
        e.doc = engine::Document::new_depth(8, 8, 16).unwrap();
        e.doc.layers[0]
            .pixels
            .set16(2, 3, [10001, 20003, 40007, 65535]);
        let shared = Arc::new(Mutex::new(e));
        save(&shared, &path).unwrap();
        let original = fs::read(&path).unwrap();
        let target = if scenario == 3 {
            dir.join("pending-save-as.psd")
        } else {
            path.clone()
        };
        let mut expected = original.clone();
        let result = save_prepared(&shared, &target, &json!({}), || match scenario {
            0 => {
                let mut e = shared.lock().unwrap();
                e.doc.layers[0]
                    .pixels
                    .set16(2, 3, [54321, 23459, 45671, 65535]);
                e.doc.revision += 1;
            }
            1 => shared.lock().unwrap().path = Some(dir.join("other.psd")),
            2 => {
                expected = b"new external version".to_vec();
                atomic_write(&target, &expected).unwrap();
            }
            3 => {
                expected = b"new destination appeared".to_vec();
                atomic_write(&target, &expected).unwrap();
            }
            _ => unreachable!(),
        });
        assert!(result.is_err());
        assert_eq!(fs::read(&target).unwrap(), expected);
        let e = shared.lock().unwrap();
        assert!(!e.closed);
        if scenario == 0 {
            assert_eq!(
                e.doc.layers[0].pixels.get16(2, 3),
                [54321, 23459, 45671, 65535]
            );
            assert_ne!(e.saved_revision, e.doc.revision);
        }
    }
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn flushed_temp_failure_preserves_original_and_cleans_incomplete_file() {
    let dir = std::env::temp_dir().join(format!("pb-interrupted-save-{}", engine::id()));
    fs::create_dir(&dir).unwrap();
    let path = dir.join("document.psd");
    fs::write(&path, b"prior complete file").unwrap();
    let result = atomic_write_checked(&path, b"new prepared file", |_| {
        Err("Injected publication failure".into())
    });
    assert!(result.is_err());
    assert_eq!(fs::read(&path).unwrap(), b"prior complete file");
    assert_eq!(fs::read_dir(&dir).unwrap().count(), 1);
    fs::remove_dir_all(dir).unwrap();
}
