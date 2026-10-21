use super::*;

type SaveObserver = Box<dyn FnMut(&str, &Path, &Path) -> Result<(), String>>;
thread_local! {
    static SAVE_OBSERVER: std::cell::RefCell<Option<SaveObserver>> = const { std::cell::RefCell::new(None) };
}
pub(super) fn observe(phase: &str, output: &Path, spool: &Path) -> Result<(), String> {
    let observer = SAVE_OBSERVER.with(|slot| slot.borrow_mut().take());
    let Some(mut observer) = observer else {
        return Ok(());
    };
    let result = observer(phase, output, spool);
    SAVE_OBSERVER.with(|slot| *slot.borrow_mut() = Some(observer));
    result
}
struct WatchSave;
impl WatchSave {
    fn new(observer: impl FnMut(&str, &Path, &Path) -> Result<(), String> + 'static) -> Self {
        SAVE_OBSERVER.with(|slot| assert!(slot.borrow_mut().replace(Box::new(observer)).is_none()));
        Self
    }
}
impl Drop for WatchSave {
    fn drop(&mut self) {
        SAVE_OBSERVER.with(|slot| slot.borrow_mut().take());
    }
}

fn native_save_fixture(depth: u16, name: &str) -> Shared {
    let mut e = Engine::new();
    e.doc = engine::Document::new_depth(4, 3, depth).unwrap();
    e.doc.name = name.into();
    e.doc.layers[0].pixels.set16(0, 0, [12347, 23459, 34571, 0]);
    e.doc.layers[0]
        .pixels
        .set16(2, 1, [50001, 30003, 10001, 32769]);
    let mut mask = raster::Raster::new_depth(4, 3, depth);
    mask.set16(2, 1, [45673, 45673, 45673, 65535]);
    e.doc.layers[0].mask = Some(engine::Mask {
        linked: true,
        enabled: false,
        cache_key: engine::id(),
        steps: vec![engine::MaskStep {
            id: engine::id(),
            kind: "paint".into(),
            enabled: true,
            weight: 1.,
            value: 0.,
            pixels: mask,
            settings: json!(null),
        }],
    });
    let layer = e.doc.layers[0].id.clone();
    e.edit(
        "human",
        &[json!({"op":"fill","layer":layer,"rect":[3,2,4,3],"color":[41,83,127,173]})],
        None,
        None,
        "Human artwork",
    )
    .unwrap();
    Arc::new(Mutex::new(e))
}

#[test]
fn streamed_working_save_keeps_native_sources_history_ownership_and_previous_bytes() {
    for depth in [8, 16] {
        for extension in ["psd", "psb"] {
            let dir = std::env::temp_dir().join(format!("pb-stream-save-{}", engine::id()));
            fs::create_dir(&dir).unwrap();
            let path = dir.join(format!("art.{extension}"));
            let shared = native_save_fixture(depth, path.file_name().unwrap().to_str().unwrap());
            let before = serde_json::to_value(&shared.lock().unwrap().doc).unwrap();
            let history = shared.lock().unwrap().undo.len();
            save(&shared, &path).unwrap();
            let original = fs::read(&path).unwrap();
            let loaded = psd::decode(&original).unwrap();
            assert!(!loaded.read_only);
            assert_eq!(serde_json::to_value(&loaded).unwrap(), before);
            let resource_length = u32::from_be_bytes(original[30..34].try_into().unwrap()) as usize;
            let mut standard = original[..30].to_vec();
            standard.extend([0; 4]);
            standard.extend(&original[34 + resource_length..]);
            let ordinary = psd::decode(&standard).unwrap();
            assert_eq!(ordinary.bit_depth, depth);
            assert_eq!(ordinary.layers.len(), 1);
            for (actual, expected) in ordinary.layers.iter().zip(&loaded.layers) {
                assert_eq!(actual.pixels.rgba16(), expected.pixels.rgba16());
                assert_eq!(
                    actual.mask.as_ref().unwrap().enabled,
                    expected.mask.as_ref().unwrap().enabled
                );
            }
            {
                let e = shared.lock().unwrap();
                assert_eq!(serde_json::to_value(&e.doc).unwrap(), before);
                assert_eq!(e.undo.len(), history);
                assert_eq!(e.saved_revision, e.doc.revision);
                assert_eq!(
                    e.file_version,
                    Some(crate::file_safety::version(&path).unwrap())
                );
                assert!(e.file_ownership.is_some());
                assert!(crate::file_ownership::Ownership::open(&path, "other project").is_err());
            }
            {
                let mut e = shared.lock().unwrap();
                let layer = e.doc.layers[0].id.clone();
                e.edit(
                    "human",
                    &[json!({"op":"fill","layer":layer,"rect":[1,0,2,1],"color":[13,29,53,97]})],
                    None,
                    None,
                    "Later artwork",
                )
                .unwrap();
            }
            save(&shared, &path).unwrap();
            let prior = crate::file_safety::previous_versions(&path).unwrap();
            assert!(prior
                .iter()
                .any(|version| fs::read(&version.path).unwrap() == original));
            let e = shared.lock().unwrap();
            assert_eq!(e.undo.len(), history + 1);
            assert_eq!(e.saved_revision, e.doc.revision);
            assert_eq!(
                psd::decode(&fs::read(&path).unwrap()).unwrap().layers[0]
                    .pixels
                    .rgba16(),
                e.doc.layers[0].pixels.rgba16()
            );
            assert!(!fs::read_dir(&dir).unwrap().any(|entry| matches!(
                entry.unwrap().path().extension().and_then(|s| s.to_str()),
                Some("tmp" | "psd-spool")
            )));
            drop(e);
            drop(shared);
            fs::remove_dir_all(dir).unwrap();
        }
    }
}

#[test]
fn streamed_save_faults_preserve_dirty_work_original_and_foreign_temporary_files() {
    for depth in [8, 16] {
        for extension in ["psd", "psb"] {
            for fault in [
                "cancel",
                "output-bytes",
                "spool-bytes",
                "foreign-output",
                "published",
            ] {
                let dir = std::env::temp_dir().join(format!("pb-stream-fault-{}", engine::id()));
                fs::create_dir(&dir).unwrap();
                let path = dir.join(format!("art.{extension}"));
                let shared =
                    native_save_fixture(depth, path.file_name().unwrap().to_str().unwrap());
                save(&shared, &path).unwrap();
                let original = fs::read(&path).unwrap();
                {
                    let mut e = shared.lock().unwrap();
                    let layer = e.doc.layers[0].id.clone();
                    e.edit("human", &[json!({"op":"fill","layer":layer,"rect":[1,0,2,1],"color":[13,29,53,97]})], None, None, "Pending artwork").unwrap();
                }
                let before = serde_json::to_value(&shared.lock().unwrap().doc).unwrap();
                let saved = shared.lock().unwrap().saved_revision;
                let version = shared.lock().unwrap().file_version;
                let history = shared.lock().unwrap().undo.len();
                let foreign = dir.join("foreign-replacement.tmp");
                let relocated = dir.join("relocated-original.tmp");
                let (foreign_observed, relocated_observed) = (foreign.clone(), relocated.clone());
                let mut count = 0;
                let watch = WatchSave::new(move |phase, output, spool| {
                    if fault == "cancel" && phase == "encoding" {
                        count += 1;
                        if count == 10 {
                            return Err("Injected save cancellation".into());
                        }
                    }
                    if phase == "encoded" {
                        match fault {
                            "output-bytes" => fs::write(output, b"changed output").unwrap(),
                            "spool-bytes" => fs::write(spool, b"changed spool").unwrap(),
                            "foreign-output" => {
                                fs::rename(output, &relocated_observed).unwrap();
                                fs::write(output, b"foreign bytes must remain").unwrap();
                                fs::write(
                                    &foreign_observed,
                                    output.as_os_str().to_string_lossy().as_bytes(),
                                )
                                .unwrap();
                            }
                            _ => {}
                        }
                    }
                    if phase == "published" && fault == "published" {
                        return Err("Injected after-rename failure".into());
                    }
                    Ok(())
                });
                let error = save(&shared, &path).unwrap_err();
                drop(watch);
                assert!(!error.is_empty());
                let e = shared.lock().unwrap();
                assert_eq!(serde_json::to_value(&e.doc).unwrap(), before);
                assert_eq!(e.saved_revision, saved);
                assert_eq!(e.file_version, version);
                assert_eq!(e.undo.len(), history);
                assert!(e.file_ownership.is_some());
                if fault == "published" {
                    assert_ne!(fs::read(&path).unwrap(), original);
                    assert!(crate::file_safety::previous_versions(&path)
                        .unwrap()
                        .iter()
                        .any(|old| fs::read(&old.path).unwrap() == original));
                } else {
                    assert_eq!(fs::read(&path).unwrap(), original);
                }
                if fault == "foreign-output" {
                    let replaced = PathBuf::from(fs::read_to_string(&foreign).unwrap());
                    assert_eq!(fs::read(replaced).unwrap(), b"foreign bytes must remain");
                    assert!(relocated.is_file());
                } else {
                    assert!(!fs::read_dir(&dir).unwrap().any(|entry| matches!(
                        entry.unwrap().path().extension().and_then(|s| s.to_str()),
                        Some("tmp" | "psd-spool")
                    )));
                }
                drop(e);
                drop(shared);
                fs::remove_dir_all(dir).unwrap();
            }
        }
    }
}

#[cfg(unix)]
#[test]
fn streamed_save_rejects_same_held_inode_symlinks_and_never_deletes_the_foreign_name() {
    for depth in [8, 16] {
        for extension in ["psd", "psb"] {
            for target in ["output", "spool"] {
                let dir = std::env::temp_dir().join(format!("pb-stream-alias-{}", engine::id()));
                fs::create_dir(&dir).unwrap();
                let path = dir.join(format!("art.{extension}"));
                let shared =
                    native_save_fixture(depth, path.file_name().unwrap().to_str().unwrap());
                save(&shared, &path).unwrap();
                let original = fs::read(&path).unwrap();
                let before = serde_json::to_value(&shared.lock().unwrap().doc).unwrap();
                let observed = Arc::new(Mutex::new(None));
                let record = observed.clone();
                let relocated = dir.join("held-native-bytes");
                let linked_target = relocated.clone();
                let watch = WatchSave::new(move |phase, output, spool| {
                    if phase == "encoded" {
                        let selected = if target == "output" { output } else { spool };
                        fs::rename(selected, &linked_target).unwrap();
                        std::os::unix::fs::symlink(&linked_target, selected).unwrap();
                        *record.lock().unwrap() = Some(selected.to_path_buf());
                    }
                    Ok(())
                });
                let error = save(&shared, &path).unwrap_err();
                drop(watch);
                assert!(error.contains("regular file"));
                assert_eq!(fs::read(&path).unwrap(), original);
                assert_eq!(
                    serde_json::to_value(&shared.lock().unwrap().doc).unwrap(),
                    before
                );
                let alias = observed.lock().unwrap().clone().unwrap();
                assert!(fs::symlink_metadata(alias)
                    .unwrap()
                    .file_type()
                    .is_symlink());
                assert!(relocated.is_file());
                drop(shared);
                fs::remove_dir_all(dir).unwrap();
            }
        }
    }
}

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
    for scenario in 0..7 {
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
            4 => shared.lock().unwrap().doc.id = engine::id(),
            5 => shared.lock().unwrap().file_version = None,
            6 => {
                let mut e = shared.lock().unwrap();
                let before = e.saved_revision;
                e.saved_revision = before.wrapping_add(1);
                assert_ne!(
                    e.saved_revision, before,
                    "Inject a real saved-marker change"
                );
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

#[test]
#[ignore = "Opt-in synthetic Save phase timings; run alone from a clean build"]
fn synthetic_save_phase_diagnostic() {
    use std::{cell::RefCell, rc::Rc, time::Instant};
    let build: Value = serde_json::from_str(crate::build_info::json()).unwrap();
    assert_eq!(build["tree"], "clean");
    assert_eq!(build["commit"].as_str().unwrap().len(), 40);
    let parent = std::env::temp_dir().canonicalize().unwrap();
    let directory = parent.join(format!("pb-save-phases-{}", engine::id()));
    fs::create_dir(&directory).unwrap();
    assert_eq!(
        directory.canonicalize().unwrap().parent(),
        Some(parent.as_path())
    );
    let mut records = Vec::new();
    for depth in [8, 16] {
        let source = directory.join(format!("source-{depth}.psd"));
        let shared = Arc::new(Mutex::new(Engine::new()));
        {
            let mut e = shared.lock().unwrap();
            e.doc = engine::Document::new_depth(2048, 2048, depth).unwrap();
            for y in 0..2048i32 {
                for x in 0..2048i32 {
                    let (x, y) = (x as u16, y as u16);
                    let alpha = if (u32::from(x) + 3 * u32::from(y)) % 17 == 0 {
                        0
                    } else {
                        12001u16
                            .wrapping_add(x.wrapping_mul(31))
                            .wrapping_add(y.wrapping_mul(47))
                    };
                    e.doc.layers[0].pixels.set16(
                        i32::from(x),
                        i32::from(y),
                        [
                            x.wrapping_mul(239)
                                .wrapping_add(y.wrapping_mul(911))
                                .wrapping_add(17),
                            x.wrapping_mul(13) ^ y.wrapping_mul(907) ^ 0x873b,
                            x.wrapping_add(y).wrapping_mul(613).wrapping_add(0x4ef3),
                            alpha,
                        ],
                    );
                }
            }
        }
        save(&shared, &source).unwrap();
        drop(shared);
        let source_version = crate::file_safety::version(&source).unwrap();
        for trial in 1..=3 {
            for extension in ["psd", "psb"] {
                let output = directory.join(format!("saved-{depth}-{trial}.{extension}"));
                let shared = Arc::new(Mutex::new(Engine::new()));
                open(&shared, &source).unwrap();
                let before = shared.lock().unwrap().doc.clone();
                assert_eq!(shared.lock().unwrap().saved_revision, before.revision);
                let history = shared.lock().unwrap().undo.len();
                #[derive(Default)]
                struct Phases {
                    source_checks: u64,
                    encoded: Option<std::time::Duration>,
                    precommit: Option<std::time::Duration>,
                    published: Option<std::time::Duration>,
                }
                let started = Instant::now();
                let phases = Rc::new(RefCell::new(Phases::default()));
                let observed = phases.clone();
                let observed_engine = shared.clone();
                let watch = WatchSave::new(move |phase, _, _| {
                    let mut p = observed.borrow_mut();
                    match phase {
                        "encoding" => p.source_checks += 1,
                        "encoded" => {
                            assert!(
                                observed_engine.try_lock().is_ok(),
                                "Encoding must leave the engine available"
                            );
                            assert!(p.encoded.replace(started.elapsed()).is_none());
                        }
                        "published" => {
                            assert!(
                                observed_engine.try_lock().is_err(),
                                "This existing checkpoint is inside the publication lock"
                            );
                            assert!(p.published.replace(started.elapsed()).is_none());
                        }
                        _ => {}
                    }
                    Ok(())
                });
                let precommit = phases.clone();
                let precommit_engine = shared.clone();
                save_prepared(&shared, &output, &json!({}), || {
                    assert!(
                        precommit_engine.try_lock().is_ok(),
                        "Verification before final publication must leave the engine available"
                    );
                    assert!(precommit
                        .borrow_mut()
                        .precommit
                        .replace(started.elapsed())
                        .is_none());
                })
                .unwrap();
                let total = started.elapsed();
                drop(watch);
                let p = phases.borrow();
                let (encoded, precommit, published) = (
                    p.encoded.unwrap(),
                    p.precommit.unwrap(),
                    p.published.unwrap(),
                );
                assert!(encoded <= precommit && precommit <= published && published <= total);
                let millis = |d: std::time::Duration| d.as_secs_f64() * 1000.;
                records.push(json!({
                    "depth":depth,"container":extension,"trial":trial,
                    "width":2048,"height":2048,"layers":1,"painted_masks":0,"effects":0,
                    "source_check_callbacks":p.source_checks,
                    "encode_ms":millis(encoded),
                    "verify_and_backup_ms":millis(precommit-encoded),
                    "precommit_to_published_ms":millis(published-precommit),
                    "published_to_return_ms":millis(total-published),
                    "precommit_to_return_ms":millis(total-precommit),
                    "engine_locked_at_published_checkpoint":true,
                    "save_ms":millis(total),"output_bytes":fs::metadata(&output).unwrap().len()
                }));
                drop(p);
                {
                    let e = shared.lock().unwrap();
                    assert_eq!(
                        (e.doc.id.as_str(), e.doc.revision, e.doc.bit_depth),
                        (before.id.as_str(), before.revision, depth)
                    );
                    assert_eq!(e.undo.len(), history);
                    assert_eq!(
                        e.doc.layers[0].pixels.rgba16(),
                        before.layers[0].pixels.rgba16()
                    );
                    assert_eq!(e.saved_revision, before.revision);
                }
                let loaded = psd::decode(&fs::read(&output).unwrap()).unwrap();
                assert!(!loaded.read_only);
                assert_eq!(loaded.bit_depth, depth);
                assert_eq!(loaded.layers[0].pixels.depth, depth);
                assert_eq!(
                    loaded.layers[0].pixels.rgba16(),
                    before.layers[0].pixels.rgba16()
                );
                assert_eq!(
                    crate::file_safety::version(&source).unwrap(),
                    source_version
                );
                drop(shared);
            }
        }
    }
    // Only this newly created, checked synthetic directory is eligible for cleanup.
    let metadata = fs::symlink_metadata(&directory).unwrap();
    assert!(metadata.is_dir() && !metadata.file_type().is_symlink());
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        assert_eq!(metadata.file_attributes() & 0x400, 0);
    }
    assert_eq!(
        directory.canonicalize().unwrap().parent(),
        Some(parent.as_path())
    );
    fs::remove_dir_all(&directory).unwrap();
    assert!(!directory.exists());
    println!(
        "\n{}",
        json!({
            "format":"PeerBrushSavePhaseDiagnosticV1","build":build,
            "native_window":false,"normal_application":false,
            "workload":"synthetic clean Save As from a native dense PSD",
            "profile":if cfg!(debug_assertions) {"test-debug-assertions"} else {"test-release"},
            "note":"Opt-in test observer overhead is included. Cache and scheduling are uncontrolled. The final phase includes post-publication verification and return cleanup. The engine is observed locked at the publication checkpoint; exact unlock time is not instrumented. This is not input-to-display latency or total lock time.",
            "records":records
        })
    );
}
