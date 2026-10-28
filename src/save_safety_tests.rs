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
fn multistrip_working_save_preserves_destination_history_and_foreign_scratch() {
    for depth in [8, 16] {
        for extension in ["psd", "psb"] {
            for fault in [
                "none",
                "cancel",
                "stale",
                "scratch-bytes",
                "foreign-scratch",
            ] {
                let dir = std::env::temp_dir().join(format!("pb-multistrip-save-{}", engine::id()));
                fs::create_dir(&dir).unwrap();
                let path = dir.join(format!("art.{extension}"));
                let mut engine = Engine::new();
                engine.doc = engine::Document::new_depth(257, 515, depth).unwrap();
                engine.doc.name = format!("art.{extension}");
                let layer = engine.doc.layers[0].id.clone();
                engine.edit("human", &[json!({"op":"fill","layer":layer,"rect":[0,0,257,515],"color":[37,73,151,193]})], None, None, "Dense native artwork").unwrap();
                let shared = Arc::new(Mutex::new(engine));
                save(&shared, &path).unwrap();
                let original = fs::read(&path).unwrap();
                {
                    let mut engine = shared.lock().unwrap();
                    engine.edit("human", &[json!({"op":"fill","layer":layer,"rect":[0,514,257,515],"color":[17,83,139,211]})], None, None, "Last strip artwork").unwrap();
                }
                let before = serde_json::to_value(&shared.lock().unwrap().doc).unwrap();
                let history = shared.lock().unwrap().undo.len();
                let saved = shared.lock().unwrap().saved_revision;
                let control = crate::saving::Control::default();
                let observed_control = control.clone();
                let observed_shared = shared.clone();
                let foreign = dir.join("owned-scratch-relocated");
                let foreign_name = Arc::new(Mutex::new(None));
                let observed_foreign = foreign_name.clone();
                let mut hit = false;
                let watch = WatchSave::new(move |phase, output, _| {
                    if !hit
                        && phase == "encoding"
                        && observed_control.status().stage == "Rendering composite strips"
                        && observed_control.status().completed > 0
                        && matches!(fault, "cancel" | "stale")
                    {
                        hit = true;
                        if fault == "cancel" {
                            assert!(observed_control.cancel());
                        } else {
                            let mut engine = observed_shared.lock().unwrap();
                            let layer = engine.doc.layers[0].id.clone();
                            engine.edit("human", &[json!({"op":"fill","layer":layer,"rect":[1,1,2,2],"color":[1,3,5,255]})], None, None, "Later human artwork").unwrap();
                        }
                    }
                    if phase == "composite-encoded"
                        && matches!(fault, "scratch-bytes" | "foreign-scratch")
                    {
                        if fault == "foreign-scratch" {
                            fs::rename(output, &foreign).unwrap();
                            *observed_foreign.lock().unwrap() = Some(output.to_path_buf());
                        }
                        fs::write(output, b"foreign scratch bytes").unwrap();
                    }
                    Ok(())
                });
                let result = save_controlled(&shared, &path, &json!({}), &control, || {});
                drop(watch);
                let engine = shared.lock().unwrap();
                assert_eq!(engine.undo.len(), history + usize::from(fault == "stale"));
                if fault == "none" {
                    result.unwrap();
                    assert_eq!(
                        serde_json::to_value(psd::decode(&fs::read(&path).unwrap()).unwrap())
                            .unwrap(),
                        before
                    );
                    assert_eq!(engine.saved_revision, engine.doc.revision);
                    assert!(crate::file_safety::previous_versions(&path)
                        .unwrap()
                        .iter()
                        .any(|old| fs::read(&old.path).unwrap() == original));
                } else {
                    assert!(result.is_err(), "{depth} {extension} {fault}");
                    assert_eq!(fs::read(&path).unwrap(), original);
                    assert_eq!(engine.saved_revision, saved);
                }
                if fault != "stale" {
                    assert_eq!(serde_json::to_value(&engine.doc).unwrap(), before);
                }
                if let Some(path) = &*foreign_name.lock().unwrap() {
                    assert_eq!(fs::read(path).unwrap(), b"foreign scratch bytes");
                }
                drop(engine);
                drop(shared);
                fs::remove_dir_all(dir).unwrap();
            }
        }
    }
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
                "foreign-spool",
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
                            "foreign-output" | "foreign-spool" => {
                                let selected = if fault == "foreign-output" {
                                    output
                                } else {
                                    spool
                                };
                                fs::rename(selected, &relocated_observed).unwrap();
                                fs::write(selected, b"foreign bytes must remain").unwrap();
                                fs::write(
                                    &foreign_observed,
                                    selected.as_os_str().to_string_lossy().as_bytes(),
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
                if matches!(fault, "foreign-output" | "foreign-spool") {
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
    use crate::workspace::lifecycle::{finish_exit, finish_update_exit, update_readiness};
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
        assert!(finish_exit(&workspace, &reviewed).is_err());
        let row = &reviewed[0];
        assert!(crate::workspace::close_in(
            &workspace,
            &row.project,
            &row.document,
            row.revision,
            false,
            "human"
        )
        .is_err());
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
            assert!(finish_exit(&workspace, &reviewed).is_err());
            let row = &reviewed[0];
            assert!(crate::workspace::close_in(
                &workspace,
                &row.project,
                &row.document,
                row.revision,
                false,
                "human"
            )
            .is_err());
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
    let mut stage_records = Vec::new();
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
                {
                    let mut engine = shared.lock().unwrap();
                    let layer = engine.doc.layers[0].id.clone();
                    engine
                        .reserve(
                            "human",
                            "Keep diagnostic source ownership",
                            vec![engine::Scope::layer(&layer)],
                        )
                        .unwrap();
                }
                let before = shared.lock().unwrap().doc.clone();
                let full_history = |engine: &Engine| {
                    let entries = |items: &[engine::History]| {
                        items.iter().map(|entry| json!({
                        "before":entry.before,"after":entry.after,"actor":entry.actor,"label":entry.label,
                        "task":entry.task,"scopes":entry.scopes,"gesture":entry.gesture,"reverted":entry.reverted
                    })).collect::<Vec<_>>()
                    };
                    json!({"undo":entries(&engine.undo),"redo":entries(&engine.redo),"leases":engine.leases})
                };
                let previous_history = full_history(&shared.lock().unwrap());
                assert!(!shared.lock().unwrap().leases.is_empty());
                assert_eq!(shared.lock().unwrap().saved_revision, before.revision);
                let history = shared.lock().unwrap().undo.len();
                #[derive(serde::Serialize)]
                struct ObservedStage {
                    stage: String,
                    first_observed_ns: u64,
                    last_observed_ns: u64,
                    source_checks: u64,
                    spool_write_callbacks: u64,
                    output_write_callbacks: u64,
                }
                #[derive(Default)]
                struct Phases {
                    source_checks: u64,
                    encoded: Option<std::time::Duration>,
                    precommit: Option<std::time::Duration>,
                    published: Option<std::time::Duration>,
                    stages: Vec<ObservedStage>,
                    observer_callbacks: u64,
                    observer_body_ns: u64,
                    maximum_observer_body_ns: u64,
                    previous_callback_ns: Option<u64>,
                    maximum_callback_gap_ns: u64,
                }
                let control = crate::saving::Control::default();
                let observed_control = control.clone();
                let started = Instant::now();
                let phases = Rc::new(RefCell::new(Phases::default()));
                let observed = phases.clone();
                let observed_engine = shared.clone();
                let watch = WatchSave::new(move |phase, output, _| {
                    let entered = Instant::now();
                    let status = observed_control.status();
                    let elapsed = started.elapsed().as_nanos() as u64;
                    let mut p = observed.borrow_mut();
                    if p.stages.last().is_none_or(|row| row.stage != status.stage) {
                        p.stages.push(ObservedStage {
                            stage: status.stage,
                            first_observed_ns: elapsed,
                            last_observed_ns: elapsed,
                            source_checks: 0,
                            spool_write_callbacks: 0,
                            output_write_callbacks: 0,
                        });
                    }
                    let row = p.stages.last_mut().unwrap();
                    row.last_observed_ns = elapsed;
                    if phase == "encoding" {
                        row.source_checks += 1;
                    }
                    if phase == "write" {
                        match output.extension().and_then(|value| value.to_str()) {
                            Some("psd-spool") => row.spool_write_callbacks += 1,
                            Some("tmp") => row.output_write_callbacks += 1,
                            _ => {
                                panic!("The existing write observer must name its owned temporary")
                            }
                        }
                    }
                    if let Some(previous) = p.previous_callback_ns.replace(elapsed) {
                        p.maximum_callback_gap_ns =
                            p.maximum_callback_gap_ns.max(elapsed - previous);
                    }
                    p.observer_callbacks += 1;
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
                    let overhead = entered.elapsed().as_nanos() as u64;
                    p.observer_body_ns += overhead;
                    p.maximum_observer_body_ns = p.maximum_observer_body_ns.max(overhead);
                    Ok(())
                });
                let precommit = phases.clone();
                let precommit_engine = shared.clone();
                save_controlled(&shared, &output, &json!({}), &control, || {
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
                let terminal_observed_ns = started.elapsed().as_nanos() as u64;
                let terminal = control.status();
                assert!(
                    terminal.finished
                        && terminal.error.is_none()
                        && !terminal.can_cancel
                        && !terminal.cancel_requested
                );
                assert_eq!(terminal.stage, "Saved");
                assert_eq!(
                    (terminal.document_id.as_str(), terminal.revision),
                    (before.id.as_str(), before.revision)
                );
                let p = phases.borrow();
                for expected in [
                    "Writing editable sources",
                    "Assembling verified Photoshop output",
                    "Verifying native spool",
                    "Verifying save output",
                    "Preserving previous version",
                    "Finishing save",
                ] {
                    assert!(
                        p.stages.iter().any(|row| row.stage == expected),
                        "Missing existing guarded stage: {expected}"
                    );
                }
                assert_eq!(
                    p.stages.iter().map(|row| row.source_checks).sum::<u64>(),
                    p.source_checks
                );
                let stage_record = json!({"depth":depth,"container":extension,"trial":trial,
                    "source_check_callbacks":p.source_checks,"observer_callbacks":p.observer_callbacks,
                    "observer_callback_body_ns":p.observer_body_ns,"maximum_observer_callback_body_ns":p.maximum_observer_body_ns,
                    "maximum_callback_gap_ns":p.maximum_callback_gap_ns,"stages":p.stages,
                    "terminal_observed_ns":terminal_observed_ns,"terminal_stage":terminal.stage,
                    "save_return_ns":total.as_nanos(),"full_source_standard_composite_history_reservations_ownership_verified":true});
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
                    assert_eq!(full_history(&e), previous_history);
                    let mut expected = before.clone();
                    expected.name = output.file_name().unwrap().to_string_lossy().into();
                    assert_eq!(
                        serde_json::to_value(&e.doc).unwrap(),
                        serde_json::to_value(&expected).unwrap()
                    );
                    assert_eq!(e.path.as_deref(), Some(output.as_path()));
                    assert!(e.file_ownership.is_some());
                    assert!(crate::file_ownership::Ownership::open(
                        &output,
                        "unrelated-diagnostic-project"
                    )
                    .is_err());
                    assert_eq!(
                        e.file_version,
                        Some(crate::file_safety::version(&output).unwrap())
                    );
                    assert_eq!(
                        e.doc.layers[0].pixels.rgba16(),
                        before.layers[0].pixels.rgba16()
                    );
                    assert_eq!(e.saved_revision, before.revision);
                }
                let bytes = fs::read(&output).unwrap();
                let loaded = psd::decode(&bytes).unwrap();
                assert_eq!(
                    serde_json::to_value(&loaded).unwrap(),
                    serde_json::to_value(&before).unwrap()
                );
                assert_eq!(loaded.export_png().unwrap(), before.export_png().unwrap());
                let resources = u32::from_be_bytes(bytes[30..34].try_into().unwrap()) as usize;
                let mut standard = bytes[..30].to_vec();
                standard.extend([0; 4]);
                standard.extend(&bytes[34 + resources..]);
                let ordinary = psd::decode(&standard).unwrap();
                assert_eq!(ordinary.bit_depth, depth);
                assert_eq!(ordinary.layers.len(), 1);
                assert_eq!(
                    ordinary.layers[0].pixels.rgba16(),
                    before.layers[0].pixels.rgba16()
                );
                assert_eq!(ordinary.export_png().unwrap(), before.export_png().unwrap());
                // Independent wire offset and native-render expectation: removing
                // PBR1 above must not substitute for the saved current composite.
                let section = 34 + resources;
                let length_bytes = if extension == "psb" { 8 } else { 4 };
                let mut encoded_length = [0u8; 8];
                encoded_length[8 - length_bytes..]
                    .copy_from_slice(&bytes[section..section + length_bytes]);
                let merged = section + length_bytes + u64::from_be_bytes(encoded_length) as usize;
                assert_eq!(&bytes[merged..merged + 2], &[0, 0]);
                let pixels = image::load_from_memory(&before.export_png().unwrap())
                    .unwrap()
                    .into_rgba16()
                    .into_raw();
                let plane = 2048 * 2048 * usize::from(depth / 8);
                assert_eq!(bytes.len() - merged - 2, 4 * plane);
                let maximum = if depth == 16 { 65535u64 } else { 255 };
                for channel in 0..4 {
                    for (index, pixel) in pixels.chunks_exact(4).enumerate() {
                        let alpha = u64::from(pixel[3]) / if depth == 16 { 1 } else { 257 };
                        let color = u64::from(pixel[channel]) / if depth == 16 { 1 } else { 257 };
                        let expected = if channel == 3 {
                            alpha
                        } else {
                            (color * alpha + maximum * (maximum - alpha) + maximum / 2) / maximum
                        };
                        let at = merged + 2 + channel * plane + index * usize::from(depth / 8);
                        if depth == 16 {
                            assert_eq!(&bytes[at..at + 2], &(expected as u16).to_be_bytes());
                        } else {
                            assert_eq!(bytes[at], expected as u8);
                        }
                    }
                }
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
                assert!(!fs::read_dir(&directory).unwrap().any(|entry| matches!(
                    entry
                        .unwrap()
                        .path()
                        .extension()
                        .and_then(|value| value.to_str()),
                    Some("tmp" | "psd-spool")
                )));
                drop(precommit_engine);
                drop(shared);
                assert!(crate::file_ownership::local_project(&output).is_none());
                stage_records.push(stage_record);
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
    println!(
        "\n{}",
        json!({"format":"PeerBrushSaveControlStageDiagnosticV1","build":build,
        "native_window":false,"normal_application":false,"periodic_polling":false,
        "observation":"Existing per-guard/write WatchSave callbacks read the actual Control; no added thread or production instrumentation",
        "limits":"First-observed stage checkpoints are not exact progress calls or syscall/lock durations. No-callback I/O/fsync spans can be charged to an adjacent stage. Callback body wall overhead is measured and included; TLS dispatch/source checks and clocks have additional unmeasured overhead. Fixed order and caches/scheduling remain uncontrolled. The in-process test does not include CLI startup/JSON/IPC, normal native UI, GPU, physical latency or before/after attribution.",
        "records":stage_records})
    );
}

#[test]
fn save_temporary_rejects_replaced_names_before_reads_writes_and_cleanup() {
    for spool in [false, true] {
        for read in [false, true] {
            let parent = std::env::temp_dir().canonicalize().unwrap();
            let directory = parent.join(format!("pb-held-save-{}", engine::id()));
            fs::create_dir(&directory).unwrap();
            let destination = directory.join("art.psd");
            let mut temporary = SaveTemporary::new(&destination, spool).unwrap();
            temporary.write_all(b"owned native bytes").unwrap();
            temporary.flush().unwrap();
            let original_identity = temporary.identity;
            let fingerprint = crate::file_safety::finish_fingerprint(temporary.fingerprint.clone());
            let bytes = temporary.bytes;
            let name = temporary.path.clone();
            let relocated = directory.join("relocated-owned-file");
            if read {
                temporary.rewind().unwrap();
            }
            fs::rename(&name, &relocated).unwrap();
            fs::write(&name, b"foreign name retained").unwrap();
            assert_eq!(
                crate::file_safety::identity(&temporary.file).unwrap(),
                original_identity
            );
            let error = if read {
                temporary.read(&mut [0; 4]).unwrap_err()
            } else {
                temporary.write(b"must not append").unwrap_err()
            };
            assert!(error.to_string().contains("identity changed"));
            assert_eq!(temporary.bytes, bytes);
            assert_eq!(
                crate::file_safety::finish_fingerprint(temporary.fingerprint.clone()),
                fingerprint
            );
            assert_eq!(fs::read(&relocated).unwrap(), b"owned native bytes");
            assert_eq!(fs::read(&name).unwrap(), b"foreign name retained");
            drop(temporary);
            assert_eq!(fs::read(&name).unwrap(), b"foreign name retained");
            assert_eq!(fs::read(&relocated).unwrap(), b"owned native bytes");
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
            fs::remove_dir_all(directory).unwrap();
        }
    }
}

#[test]
fn save_temporary_fingerprint_tracks_only_written_bytes_and_survives_reads() {
    let parent = std::env::temp_dir().canonicalize().unwrap();
    let directory = parent.join(format!("pb-stream-fingerprint-{}", engine::id()));
    fs::create_dir(&directory).unwrap();
    let destination = directory.join("art.psd");
    let bytes = vec![b'a'; 1_000_000];
    for spool in [false, true] {
        let mut temporary = SaveTemporary::new(&destination, spool).unwrap();
        for chunk in bytes.chunks(4093) {
            let mut remaining = chunk;
            while !remaining.is_empty() {
                let written = temporary.write(remaining).unwrap();
                assert!(written > 0 && written <= remaining.len());
                remaining = &remaining[written..];
            }
        }
        let expected = crate::file_safety::finish_fingerprint(temporary.fingerprint.clone());
        assert_eq!(
            expected
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>(),
            "cdc76e5c9914fb9281a1c7e284d73e67f1809a48a497200e046d39ccc7112cd0"
        );
        assert_eq!(temporary.bytes, bytes.len() as u64);
        assert_eq!(
            temporary.verify(|| Ok(())).unwrap(),
            crate::file_safety::published_version(&temporary.path, &bytes).unwrap()
        );
        temporary.rewind().unwrap();
        let mut readback = Vec::new();
        temporary.read_to_end(&mut readback).unwrap();
        assert_eq!(readback, bytes);
        let error = temporary.write(b"rejected after read").unwrap_err();
        assert!(error.to_string().contains("not sequential"));
        assert_eq!(temporary.bytes, bytes.len() as u64);
        assert_eq!(
            crate::file_safety::finish_fingerprint(temporary.fingerprint.clone()),
            expected
        );
        assert_eq!(fs::read(&temporary.path).unwrap(), bytes);
    }
    assert_eq!(fs::read_dir(&directory).unwrap().count(), 0);
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
    fs::remove_dir(directory).unwrap();
}

#[test]
fn controlled_save_cancellation_and_writer_errors_preserve_native_work_and_originals() {
    for depth in [8, 16] {
        for extension in ["psd", "psb"] {
            for selected in [
                "Preparing layer masks",
                "Writing standard layer channels",
                "Writing editable sources",
                "Assembling verified Photoshop output",
                "Verifying native spool",
                "Verifying save output",
                "writer failure",
                "readback corruption",
            ] {
                let directory =
                    std::env::temp_dir().join(format!("pb-save-control-{}", engine::id()));
                fs::create_dir(&directory).unwrap();
                let path = directory.join(format!("art.{extension}"));
                let shared =
                    native_save_fixture(depth, path.file_name().unwrap().to_str().unwrap());
                // A larger canvas keeps real verification in multiple 64 KiB reads;
                // the small editable layer/mask and its native hidden samples stay intact.
                {
                    let mut e = shared.lock().unwrap();
                    e.doc.width = 257;
                    e.doc.height = 257;
                }
                save(&shared, &path).unwrap();
                let original = fs::read(&path).unwrap();
                {
                    let mut e = shared.lock().unwrap();
                    let layer = e.doc.layers[0].id.clone();
                    e.edit("human", &[json!({"op":"fill","layer":layer,"rect":[1,0,2,1],"color":[13,29,53,97]})], None, None, "Unsaved native work").unwrap();
                }
                let (document, saved, version, history) = {
                    let e = shared.lock().unwrap();
                    (
                        serde_json::to_value(&e.doc).unwrap(),
                        e.saved_revision,
                        e.file_version,
                        e.undo.len(),
                    )
                };
                let control = crate::saving::Control::default();
                let observed_control = control.clone();
                let hit = std::rc::Rc::new(std::cell::Cell::new(false));
                let observed_hit = hit.clone();
                let mut read_checks = 0;
                let watch = WatchSave::new(move |phase, output, _| {
                    let status = observed_control.status();
                    if selected == "writer failure"
                        && phase == "write"
                        && fs::metadata(output).is_ok_and(|m| m.len() > 0)
                    {
                        observed_hit.set(true);
                        return Err("Injected save writer failure".into());
                    }
                    if selected == "readback corruption" && phase == "encoded" {
                        let mut file = fs::OpenOptions::new().write(true).open(output).unwrap();
                        file.write_all(b"BAD!").unwrap();
                        file.sync_all().unwrap();
                        observed_hit.set(true);
                    }
                    if phase == "encoding" && status.stage == selected {
                        if selected.starts_with("Verifying") {
                            read_checks += 1;
                        }
                        if matches!(
                            selected,
                            "Preparing layer masks"
                                | "Writing editable sources"
                                | "Assembling verified Photoshop output"
                        ) || (selected == "Writing standard layer channels"
                            && status.completed >= 2)
                            || (selected.starts_with("Verifying") && read_checks == 3)
                        {
                            assert!(observed_control.cancel());
                            observed_hit.set(true);
                        }
                    }
                    Ok(())
                });
                let error =
                    save_controlled(&shared, &path, &json!({}), &control, || {}).unwrap_err();
                drop(watch);
                assert!(hit.get(), "The selected real save phase must be reached");
                assert!(
                    error.contains(match selected {
                        "writer failure" => "writer failure",
                        "readback corruption" => "verification",
                        _ => "canceled",
                    }),
                    "{selected}: {error}"
                );
                assert_eq!(fs::read(&path).unwrap(), original);
                let e = shared.lock().unwrap();
                assert_eq!(serde_json::to_value(&e.doc).unwrap(), document);
                assert_eq!(e.saved_revision, saved);
                assert_eq!(e.file_version, version);
                assert_eq!(e.undo.len(), history);
                assert!(e.file_ownership.is_some());
                assert_eq!(e.file_operations.load(Ordering::Acquire), 0);
                assert!(control.status().finished && control.status().error.is_some());
                assert_eq!(
                    control.status().cancel_requested,
                    !matches!(selected, "writer failure" | "readback corruption")
                );
                assert!(!control.cancel());
                assert!(!fs::read_dir(&directory).unwrap().any(|entry| matches!(
                    entry.unwrap().path().extension().and_then(|s| s.to_str()),
                    Some("tmp" | "psd-spool")
                )));
                drop(e);
                drop(shared);
                fs::remove_dir_all(directory).unwrap();
            }
        }
    }
}

#[test]
fn controlled_save_last_cancel_is_atomic_and_late_cancel_cannot_hide_publication() {
    for depth in [8, 16] {
        for extension in ["psd", "psb"] {
            for late in [false, true] {
                let directory =
                    std::env::temp_dir().join(format!("pb-save-boundary-{}", engine::id()));
                fs::create_dir(&directory).unwrap();
                let path = directory.join(format!("art.{extension}"));
                let shared =
                    native_save_fixture(depth, path.file_name().unwrap().to_str().unwrap());
                save(&shared, &path).unwrap();
                let original = fs::read(&path).unwrap();
                {
                    let mut e = shared.lock().unwrap();
                    let layer = e.doc.layers[0].id.clone();
                    e.edit("human", &[json!({"op":"fill","layer":layer,"rect":[1,0,2,1],"color":[13,29,53,97]})], None, None, "Pending work").unwrap();
                }
                let (document, saved, history) = {
                    let e = shared.lock().unwrap();
                    (
                        serde_json::to_value(&e.doc).unwrap(),
                        e.saved_revision,
                        e.undo.len(),
                    )
                };
                let control = crate::saving::Control::default();
                let observed_control = control.clone();
                let published = std::rc::Rc::new(std::cell::Cell::new(false));
                let observed_published = published.clone();
                let watch = WatchSave::new(move |phase, _, _| {
                    if phase == "published" {
                        assert!(late);
                        assert_eq!(observed_control.status().stage, "Finishing save");
                        assert!(!observed_control.cancel());
                        observed_published.set(true);
                    }
                    Ok(())
                });
                let result = save_controlled(&shared, &path, &json!({}), &control, || {
                    if !late {
                        assert!(control.cancel());
                    }
                });
                drop(watch);
                assert_eq!(result.is_ok(), late);
                assert_eq!(published.get(), late);
                let e = shared.lock().unwrap();
                assert_eq!(serde_json::to_value(&e.doc).unwrap(), document);
                assert_eq!(e.undo.len(), history);
                assert_eq!(e.saved_revision, if late { e.doc.revision } else { saved });
                assert_eq!(fs::read(&path).unwrap() == original, !late);
                if late {
                    let decoded = psd::decode(&fs::read(&path).unwrap()).unwrap();
                    assert!(!decoded.read_only);
                    assert_eq!(
                        decoded.layers[0].pixels.rgba16(),
                        e.doc.layers[0].pixels.rgba16()
                    );
                    assert!(crate::file_safety::previous_versions(&path)
                        .unwrap()
                        .iter()
                        .any(|old| fs::read(&old.path).unwrap() == original));
                }
                assert!(control.status().finished);
                assert_eq!(control.status().error.is_none(), late);
                drop(e);
                drop(shared);
                fs::remove_dir_all(directory).unwrap();
            }
        }
    }
}

#[test]
fn save_run_binding_duplicate_registration_and_failed_spawn_keep_operations_owned() {
    let directory = std::env::temp_dir().join(format!("pb-save-run-{}", engine::id()));
    fs::create_dir(&directory).unwrap();
    let path = directory.join("art.psd");
    let shared = native_save_fixture(16, "art.psd");
    let (document, revision) = {
        let e = shared.lock().unwrap();
        (e.doc.id.clone(), e.doc.revision)
    };
    let request = json!({"document_id":document,"expected_revision":revision});
    let source = serde_json::to_value(&shared.lock().unwrap().doc).unwrap();
    assert!(save_async(&shared, path.clone(), &json!({})).is_err());
    assert!(SaveOperation::begin(
        &shared,
        &json!({"document_id":"stale"}),
        &crate::saving::Control::default()
    )
    .is_err());
    assert!(SaveOperation::begin(
        &shared,
        &json!({"document_id":document,"expected_revision":revision+1}),
        &crate::saving::Control::default()
    )
    .is_err());
    let control = crate::saving::Control::default();
    let mut operation = SaveOperation::begin(&shared, &request, &control).unwrap();
    assert_eq!(
        shared
            .lock()
            .unwrap()
            .file_operations
            .load(Ordering::Acquire),
        1
    );
    assert!(save_async(&shared, path.clone(), &request)
        .unwrap_err()
        .contains("already active"));
    assert!(dispatch(
        &shared,
        "document",
        &json!({"action":"cancel_save","document_id":document,"run":"old-run"})
    )
    .is_err());
    assert!(!control.status().cancel_requested);
    let reply = dispatch(
        &shared,
        "document",
        &json!({"action":"cancel_save","document_id":document,"run":control.status().run}),
    )
    .unwrap();
    assert_eq!(reply["cancel_accepted"], true);
    let result = control.check();
    operation.complete(&result);
    drop(operation);
    let watch = WatchSave::new(|phase, _, _| {
        if phase == "spawn" {
            Err("Injected save worker creation failure".into())
        } else {
            Ok(())
        }
    });
    assert!(save_async(&shared, path.clone(), &request)
        .unwrap_err()
        .contains("creation failure"));
    drop(watch);
    let e = shared.lock().unwrap();
    assert_eq!(e.file_operations.load(Ordering::Acquire), 0);
    assert_eq!(serde_json::to_value(&e.doc).unwrap(), source);
    assert!(e.saving.as_ref().unwrap().status().finished);
    assert!(e
        .saving
        .as_ref()
        .unwrap()
        .status()
        .error
        .as_deref()
        .unwrap()
        .contains("interrupted"));
    assert!(!path.exists());
    drop(e);
    drop(shared);
    fs::remove_dir_all(directory).unwrap();
}

#[test]
fn later_human_edit_fails_controlled_save_without_marking_it_saved_or_canceled() {
    let directory = std::env::temp_dir().join(format!("pb-save-stale-control-{}", engine::id()));
    fs::create_dir(&directory).unwrap();
    let path = directory.join("art.psb");
    let shared = native_save_fixture(16, "art.psb");
    save(&shared, &path).unwrap();
    let original = fs::read(&path).unwrap();
    let saved = shared.lock().unwrap().saved_revision;
    let history = shared.lock().unwrap().undo.len();
    let control = crate::saving::Control::default();
    let error = save_controlled(&shared, &path, &json!({}), &control, || {
        let mut e = shared.lock().unwrap();
        let layer = e.doc.layers[0].id.clone();
        e.edit(
            "human",
            &[json!({"op":"fill","layer":layer,"rect":[1,0,2,1],"color":[13,29,53,97]})],
            None,
            None,
            "Later human work",
        )
        .unwrap();
    })
    .unwrap_err();
    assert!(error.contains("source changed"));
    assert_eq!(fs::read(&path).unwrap(), original);
    let e = shared.lock().unwrap();
    assert_eq!(e.undo.len(), history + 1);
    assert_eq!(e.saved_revision, saved);
    assert_ne!(e.saved_revision, e.doc.revision);
    assert_eq!(
        e.doc.layers[0].pixels.get16(1, 0),
        [13 * 257, 29 * 257, 53 * 257, 97 * 257]
    );
    assert!(control.status().finished && control.status().error.is_some());
    assert!(!control.status().cancel_requested);
    drop(e);
    drop(shared);
    fs::remove_dir_all(directory).unwrap();
}

#[test]
fn queued_save_rejects_early_cancel_or_newer_human_source_before_serialization() {
    for depth in [8, 16] {
        for cancel in [false, true] {
            let directory = std::env::temp_dir().join(format!("pb-queued-save-{}", engine::id()));
            fs::create_dir(&directory).unwrap();
            let path = directory.join("queued.psd");
            let shared = native_save_fixture(depth, "queued.psd");
            let control = crate::saving::Control::default();
            let mut operation = SaveOperation::begin(&shared, &json!({}), &control).unwrap();
            if cancel {
                assert!(control.cancel());
            } else {
                let mut e = shared.lock().unwrap();
                let layer = e.doc.layers[0].id.clone();
                e.edit(
                    "human",
                    &[json!({"op":"fill","layer":layer,"rect":[1,0,2,1],"color":[13,29,53,97]})],
                    None,
                    None,
                    "Work after async registration",
                )
                .unwrap();
            }
            let (before, saved, history) = {
                let e = shared.lock().unwrap();
                (
                    serde_json::to_value(&e.doc).unwrap(),
                    e.saved_revision,
                    e.undo.len(),
                )
            };
            let result = save_prepared_inner(&shared, &path, &json!({}), &control, || {
                panic!("No stale/canceled save may reach publication")
            });
            assert!(result.as_ref().unwrap_err().contains(if cancel {
                "canceled"
            } else {
                "source changed before saving"
            }));
            operation.complete(&result);
            drop(operation);
            let e = shared.lock().unwrap();
            assert_eq!(serde_json::to_value(&e.doc).unwrap(), before);
            assert_eq!(e.saved_revision, saved);
            assert_eq!(e.undo.len(), history);
            assert_eq!(e.file_operations.load(Ordering::Acquire), 0);
            assert!(!e.closed);
            assert_ne!(e.doc.revision, e.saved_revision);
            assert!(control.status().finished && control.status().error.is_some());
            assert_eq!(control.status().cancel_requested, cancel);
            assert!(!path.exists());
            assert!(!fs::read_dir(&directory).unwrap().any(|entry| matches!(
                entry.unwrap().path().extension().and_then(|s| s.to_str()),
                Some("tmp" | "psd-spool")
            )));
            drop(e);
            drop(shared);
            fs::remove_dir_all(directory).unwrap();
        }
    }
}

#[test]
fn source_compression_and_replay_refuse_newer_human_work_without_publication() {
    for depth in [8, 16] {
        for extension in ["psd", "psb"] {
            for selected in [
                "Writing editable sources",
                "Assembling verified Photoshop output",
            ] {
                let parent = std::env::temp_dir().canonicalize().unwrap();
                let directory = parent.join(format!("pb-source-spool-{}", engine::id()));
                fs::create_dir(&directory).unwrap();
                assert_eq!(
                    directory.canonicalize().unwrap().parent(),
                    Some(parent.as_path())
                );
                let path = directory.join(format!("art.{extension}"));
                let shared =
                    native_save_fixture(depth, path.file_name().unwrap().to_str().unwrap());
                save(&shared, &path).unwrap();
                let original = fs::read(&path).unwrap();
                let (saved, version) = {
                    let e = shared.lock().unwrap();
                    (e.saved_revision, e.file_version)
                };
                let control = crate::saving::Control::default();
                let observed = control.clone();
                let actual = shared.clone();
                let hit = std::rc::Rc::new(std::cell::Cell::new(false));
                let reached = hit.clone();
                let retained = std::rc::Rc::new(std::cell::RefCell::new(None));
                let expected = retained.clone();
                let history = |e: &Engine| {
                    let entries = |items: &[engine::History]| {
                        items
                            .iter()
                            .map(|h| {
                                json!({"before":h.before,"after":h.after,
                            "actor":h.actor,"label":h.label,"task":h.task,"scopes":h.scopes,
                            "gesture":h.gesture,"reverted":h.reverted})
                            })
                            .collect::<Vec<_>>()
                    };
                    json!({"undo":entries(&e.undo),"redo":entries(&e.redo),"leases":e.leases})
                };
                let mut source_start = None;
                let watch = WatchSave::new(move |phase, output, spool| {
                    if phase != "encoding" || observed.status().stage != selected || reached.get() {
                        return Ok(());
                    }
                    let started = if selected == "Writing editable sources" {
                        let bytes = fs::metadata(spool).unwrap().len();
                        bytes > *source_start.get_or_insert(bytes)
                    } else {
                        // Header + PBR1 prefix are 62 bytes; a larger output
                        // reaches the real compressed-source replay, before standards.
                        fs::metadata(output).unwrap().len() > 62
                    };
                    if started {
                        reached.set(true);
                        let mut e = actual.lock().unwrap();
                        let layer = e.doc.layers[0].id.clone();
                        let revision = e.doc.revision;
                        e.edit(
                            "human",
                            &[json!({"op":"layer.update","layer":layer,"opacity":0.5})],
                            Some(revision),
                            None,
                            "Human edit during source transfer",
                        )
                        .unwrap();
                        *expected.borrow_mut() =
                            Some((serde_json::to_value(&e.doc).unwrap(), history(&e)));
                    }
                    Ok(())
                });
                let error =
                    save_controlled(&shared, &path, &json!({}), &control, || {}).unwrap_err();
                drop(watch);
                assert!(
                    hit.get(),
                    "The actual selected source phase must be reached"
                );
                assert!(error.contains("source changed"), "{selected}: {error}");
                assert_eq!(fs::read(&path).unwrap(), original);
                let e = shared.lock().unwrap();
                let expected = retained.borrow();
                let (document, histories) = expected.as_ref().unwrap();
                assert_eq!(&serde_json::to_value(&e.doc).unwrap(), document);
                assert_eq!(&history(&e), histories);
                assert_eq!(e.saved_revision, saved);
                assert_eq!(e.file_version, version);
                assert_ne!(e.saved_revision, e.doc.revision);
                assert!(e.file_ownership.is_some());
                assert!(crate::file_ownership::Ownership::open(&path, "other project").is_err());
                assert_eq!(e.file_operations.load(Ordering::Acquire), 0);
                assert!(control.status().finished && control.status().error.is_some());
                assert!(!fs::read_dir(&directory).unwrap().any(|entry| matches!(
                    entry.unwrap().path().extension().and_then(|s| s.to_str()),
                    Some("tmp" | "psd-spool")
                )));
                drop(e);
                drop(shared);
                fs::remove_dir_all(directory).unwrap();
            }
        }
    }
}

#[test]
fn buffered_source_flush_failure_and_cancel_keep_actual_native_save_and_history() {
    for depth in [8, 16] {
        for extension in ["psd", "psb"] {
            for cancel in [false, true] {
                let directory =
                    std::env::temp_dir().join(format!("pb-source-flush-{}", engine::id()));
                fs::create_dir(&directory).unwrap();
                let path = directory.join(format!("art.{extension}"));
                let shared =
                    native_save_fixture(depth, path.file_name().unwrap().to_str().unwrap());
                save(&shared, &path).unwrap();
                let original = fs::read(&path).unwrap();
                {
                    let mut e = shared.lock().unwrap();
                    let layer = e.doc.layers[0].id.clone();
                    for color in [[13, 29, 53, 97], [19, 31, 61, 113]] {
                        e.edit(
                            "human",
                            &[json!({"op":"fill","layer":layer,"rect":[1,0,2,1],"color":color})],
                            None,
                            None,
                            "Human artwork before Save",
                        )
                        .unwrap();
                    }
                    e.undo("human").unwrap();
                    assert!(!e.undo.is_empty() && !e.redo.is_empty());
                }
                let history = |e: &Engine| {
                    let entries = |items: &[engine::History]| {
                        items
                            .iter()
                            .map(|h| {
                                json!({"before":h.before,"after":h.after,
                            "actor":h.actor,"label":h.label,"task":h.task,"scopes":h.scopes,
                            "gesture":h.gesture,"reverted":h.reverted})
                            })
                            .collect::<Vec<_>>()
                    };
                    json!({"undo":entries(&e.undo),"redo":entries(&e.redo),"leases":e.leases})
                };
                let (document, histories, saved, version) = {
                    let e = shared.lock().unwrap();
                    (
                        serde_json::to_value(&e.doc).unwrap(),
                        history(&e),
                        e.saved_revision,
                        e.file_version,
                    )
                };
                let control = crate::saving::Control::default();
                let observed = control.clone();
                let hit = std::rc::Rc::new(std::cell::Cell::new(false));
                let reached = hit.clone();
                let watch = WatchSave::new(move |phase, temporary, _| {
                    if phase == "write"
                        && observed.status().stage == "Writing editable sources"
                        && temporary.extension().is_some_and(|e| e == "psd-spool")
                        && !reached.replace(true)
                    {
                        if cancel {
                            assert!(observed.cancel());
                        } else {
                            return Err("Injected compressed source physical flush failure".into());
                        }
                    }
                    Ok(())
                });
                let error =
                    save_controlled(&shared, &path, &json!({}), &control, || {}).unwrap_err();
                drop(watch);
                assert!(
                    hit.get(),
                    "The real guarded compressed-source file writer must be reached: depth={depth}, format={extension}, cancel={cancel}: {error}"
                );
                assert!(
                    error.contains(if cancel {
                        "canceled"
                    } else {
                        "physical flush failure"
                    }),
                    "{error}"
                );
                assert_eq!(fs::read(&path).unwrap(), original);
                let e = shared.lock().unwrap();
                assert_eq!(serde_json::to_value(&e.doc).unwrap(), document);
                assert_eq!(history(&e), histories);
                assert_eq!(e.saved_revision, saved);
                assert_eq!(e.file_version, version);
                assert!(e.file_ownership.is_some());
                assert!(crate::file_ownership::Ownership::open(&path, "other project").is_err());
                assert_eq!(e.file_operations.load(Ordering::Acquire), 0);
                assert!(control.status().finished && control.status().error.is_some());
                assert_eq!(control.status().cancel_requested, cancel);
                assert!(!fs::read_dir(&directory).unwrap().any(|entry| matches!(
                    entry.unwrap().path().extension().and_then(|s| s.to_str()),
                    Some("tmp" | "psd-spool")
                )));
                drop(e);
                drop(shared);
                fs::remove_dir_all(directory).unwrap();
            }
        }
    }
}

#[test]
#[ignore = "Opt-in owned-file IO component timings; no normal Save or latency claim"]
fn save_temporary_io_component_diagnostic() {
    use std::time::Instant;
    const CHUNK: usize = 64 * 1024;
    const OPERATIONS: usize = 32;
    fn elapsed(operation: impl FnOnce()) -> u64 {
        let started = Instant::now();
        operation();
        started.elapsed().as_nanos().try_into().unwrap()
    }
    let state = |engine: &Engine| {
        let entries = |items: &[engine::History]| {
            items.iter().map(|entry| json!({
            "before":entry.before,"after":entry.after,"actor":entry.actor,"label":entry.label,
            "task":entry.task,"scopes":entry.scopes,"gesture":entry.gesture,"reverted":entry.reverted
        })).collect::<Vec<_>>()
        };
        json!({"doc":engine.doc,"undo":entries(&engine.undo),"redo":entries(&engine.redo),
            "leases":engine.leases,"path":engine.path,"saved_revision":engine.saved_revision})
    };
    let build: Value = serde_json::from_str(crate::build_info::json()).unwrap();
    assert_eq!(build["tree"], "clean");
    let parent = std::env::temp_dir().canonicalize().unwrap();
    let directory = parent.join(format!("pb-save-io-components-{}", engine::id()));
    fs::create_dir(&directory).unwrap();
    assert_eq!(
        directory.canonicalize().unwrap().parent(),
        Some(parent.as_path())
    );
    let mut records = Vec::new();
    for depth in [8, 16] {
        let shared = native_save_fixture(depth, "Native IO component fixture");
        let before = state(&shared.lock().unwrap());
        let document = shared.lock().unwrap().doc.clone();
        let encoded = psd::encode(&document).unwrap();
        assert!(!encoded.is_empty() && encoded.len() <= CHUNK * OPERATIONS);
        assert_eq!(
            serde_json::to_value(psd::decode(&encoded).unwrap()).unwrap(),
            serde_json::to_value(&document).unwrap()
        );
        // Repeated encoded fixture bytes are IO data, not a published Photoshop file.
        let bytes: Vec<_> = encoded
            .iter()
            .copied()
            .cycle()
            .take(CHUNK * OPERATIONS)
            .collect();
        let expected: [u8; 32] = ring::digest::digest(&ring::digest::SHA256, &bytes)
            .as_ref()
            .try_into()
            .unwrap();
        for trial in 1..=3 {
            for spool in [false, true] {
                let mut temporary =
                    SaveTemporary::new(&directory.join("unused-destination.psd"), spool).unwrap();
                let path = temporary.path.clone();
                let control = crate::saving::Control::default();
                control
                    .begin(document.id.clone(), document.revision)
                    .unwrap();
                let cancellation_check_ns = elapsed(|| {
                    for _ in 0..OPERATIONS {
                        control.check().unwrap();
                    }
                });
                let mut writes = 0;
                let guarded_write_ns = elapsed(|| {
                    for chunk in bytes.chunks(CHUNK) {
                        let mut pending = chunk;
                        while !pending.is_empty() {
                            let n = temporary.write(pending).unwrap();
                            assert!(n > 0 && n <= pending.len());
                            writes += 1;
                            pending = &pending[n..];
                        }
                    }
                });
                assert_eq!(temporary.bytes, bytes.len() as u64);
                assert_eq!(
                    crate::file_safety::finish_fingerprint(temporary.fingerprint.clone()),
                    expected
                );
                let check_path_ns = elapsed(|| {
                    for _ in 0..OPERATIONS {
                        temporary.check_path().unwrap();
                    }
                });
                let symlink_metadata_ns = elapsed(|| {
                    for _ in 0..OPERATIONS {
                        assert!(fs::symlink_metadata(&path).unwrap().is_file());
                    }
                });
                let named_open_close_ns = elapsed(|| {
                    for _ in 0..OPERATIONS {
                        drop(crate::file_safety::open_for_identity(&path).unwrap());
                    }
                });
                let held_identity_ns = elapsed(|| {
                    for _ in 0..OPERATIONS {
                        assert_eq!(
                            crate::file_safety::identity(&temporary.file).unwrap(),
                            temporary.identity
                        );
                    }
                });
                let named = crate::file_safety::open_for_identity(&path).unwrap();
                let named_identity_ns = elapsed(|| {
                    for _ in 0..OPERATIONS {
                        assert_eq!(
                            crate::file_safety::identity(&named).unwrap(),
                            temporary.identity
                        );
                    }
                });
                drop(named);
                let mut fingerprint = ring::digest::Context::new(&ring::digest::SHA256);
                let sha256_update_ns = elapsed(|| {
                    for chunk in bytes.chunks(CHUNK) {
                        fingerprint.update(chunk);
                    }
                });
                assert_eq!(
                    crate::file_safety::finish_fingerprint(fingerprint),
                    expected
                );
                let mut verified = None;
                let verify_flush_sync_full_hash_ns = elapsed(|| {
                    verified = Some(temporary.verify(|| control.check()).unwrap());
                });
                assert_eq!(
                    verified.unwrap(),
                    crate::file_safety::published_version(&path, &bytes).unwrap()
                );
                temporary.rewind().unwrap();
                let mut readback = vec![0; bytes.len()];
                let mut reads = 0;
                let guarded_read_ns = elapsed(|| {
                    let mut offset = 0;
                    while offset < readback.len() {
                        let end = (offset + CHUNK).min(readback.len());
                        let n = temporary.read(&mut readback[offset..end]).unwrap();
                        assert!(n > 0 && n <= end - offset);
                        reads += 1;
                        offset += n;
                    }
                });
                assert_eq!(readback, bytes);
                assert!(writes >= OPERATIONS && reads >= OPERATIONS);
                assert!(control.cancel());
                assert!(temporary
                    .verify(|| control.check())
                    .unwrap_err()
                    .contains("Save canceled"));
                assert!(temporary
                    .write(b"must not append after reading")
                    .unwrap_err()
                    .to_string()
                    .contains("not sequential"));
                assert_eq!(temporary.bytes, bytes.len() as u64);
                assert_eq!(
                    crate::file_safety::finish_fingerprint(temporary.fingerprint.clone()),
                    expected
                );
                assert_eq!(fs::read(&path).unwrap(), bytes);
                drop(temporary);
                assert!(!path.exists());
                records.push(json!({"depth":depth,"spool":spool,"trial":trial,"bytes":bytes.len(),
                    "component_operations":OPERATIONS,"maximum_transfer_bytes":CHUNK,"write_calls":writes,"read_calls":reads,
                    "check_path_ns":check_path_ns,"symlink_metadata_ns":symlink_metadata_ns,
                    "named_open_close_ns":named_open_close_ns,"held_identity_ns":held_identity_ns,
                    "named_identity_ns":named_identity_ns,"guarded_write_ns":guarded_write_ns,
                    "guarded_read_ns":guarded_read_ns,"sha256_update_ns":sha256_update_ns,
                    "cancellation_check_ns":cancellation_check_ns,"verify_flush_sync_full_hash_ns":verify_flush_sync_full_hash_ns,
                    "bytes_fingerprint_cancel_read_only_transition_and_owned_cleanup_verified":true}));
            }
        }
        assert_eq!(state(&shared.lock().unwrap()), before);
    }
    // Reuse the actual four role/read-write/Drop foreign-name cases outside timing.
    save_temporary_rejects_replaced_names_before_reads_writes_and_cleanup();
    assert_eq!(fs::read_dir(&directory).unwrap().count(), 0);
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
    fs::remove_dir(&directory).unwrap();
    assert!(!directory.exists());
    assert_eq!(records.len(), 12);
    println!(
        "\n{}",
        json!({"format":"PeerBrushSaveIoComponentDiagnosticV1","build":build,
        "normal_application":false,"native_window":false,"payload_is_published_psd":false,
        "native_fixture_source_history_verified":true,"existing_real_foreign_name_read_write_drop_cases_verified":true,
        "limits":"IO-only repeated native-fixture bytes; real guards unchanged. Aggregate component samples include timer/assert overhead and overlap; do not sum as Save attribution. Guarded IO includes identity, source-independent file checks and SHA; verification includes flush/fsync/full readback. Cancellation check excludes engine/source locking. Wire FNV is not isolated. Cache, filesystem and security scanning/scheduling uncontrolled; no normal Save, native input, device latency or performance change claim.",
        "records":records})
    );
}
