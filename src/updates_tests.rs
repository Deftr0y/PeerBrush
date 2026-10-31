use super::*;
use serde_json::{json, Value};
use std::{
    fs,
    sync::{Arc, Mutex},
};

#[cfg(windows)]
#[test]
fn windows_ownership_keeps_store_priority_and_queries_association_only_for_supported_signatures() {
    use ::windows::ApplicationModel::PackageSignatureKind as Signature;
    for (signature, expected) in [
        (Ok(Signature::Store), Owner::MicrosoftStore),
        (Ok(Signature::None), Owner::WindowsPackage),
        (Ok(Signature::System), Owner::WindowsPackage),
        (Ok(Signature(i32::MAX)), Owner::WindowsPackage),
        (
            Err(::windows::core::Error::from_hresult(
                ::windows::core::HRESULT(0x80004005_u32 as i32),
            )),
            Owner::Unknown,
        ),
    ] {
        assert_eq!(
            windows_package_owner(signature, || panic!("association lookup must not run")),
            expected
        );
    }
    for signature in [Signature::Developer, Signature::Enterprise] {
        for associated in [false, true] {
            let mut queries = 0;
            let owner = windows_package_owner(Ok(signature), || {
                queries += 1;
                Ok(associated)
            });
            assert_eq!(queries, 1);
            assert_eq!(
                owner,
                if associated {
                    Owner::WindowsAppInstaller
                } else {
                    Owner::WindowsPackage
                }
            );
        }
        assert_eq!(
            windows_package_owner(Ok(signature), || Err(::windows::core::Error::from_hresult(
                ::windows::core::HRESULT(0x80004005_u32 as i32)
            ))),
            Owner::Unknown
        );
    }
}

fn release(version: &str, filename: &str) -> Value {
    json!({"draft":false,"prerelease":false,"tag_name":format!("v{version}"),
        "html_url":format!("https://github.com/Deftr0y/PeerBrush/releases/tag/v{version}"),
        "published_at":"2026-10-19T00:00:00Z", "body":"Verified release notes",
        "assets":[{"name":filename,"size":1024,"state":"uploaded",
            "browser_download_url":format!("https://github.com/Deftr0y/PeerBrush/releases/download/v{version}/{filename}")} ]})
}
fn parse(
    releases: Value,
    current: &str,
    channel: Channel,
    target: Target,
) -> Result<Report, String> {
    parse_releases(
        &serde_json::to_vec(&releases).unwrap(),
        current,
        channel,
        target,
        1,
    )
}

#[test]
fn release_selection_obeys_semver_channel_architecture_and_published_state() {
    let stable = release("0.30.0", "PeerBrush-0.30.0-Windows-x64.zip");
    let mut preview = release("0.31.0-rc.1", "PeerBrush-0.31.0-rc.1-Windows-x64.zip");
    preview["prerelease"] = json!(true);
    let mut draft = release("9.0.0", "PeerBrush-9.0.0-Windows-x64.zip");
    draft["draft"] = json!(true);
    let mut unpublished = release("8.0.0", "PeerBrush-8.0.0-Windows-x64.zip");
    unpublished["published_at"] = Value::Null;
    let data = json!([preview, draft, stable, unpublished]);
    let report = parse(
        data.clone(),
        "0.30.0-dev.1",
        Channel::Stable,
        Target::WindowsX64,
    )
    .unwrap();
    assert_eq!(report.release_version.as_deref(), Some("0.30.0"));
    assert_eq!(report.assets.len(), 1);
    let report = parse(data.clone(), "0.30.0", Channel::Preview, Target::WindowsX64).unwrap();
    assert_eq!(report.release_version.as_deref(), Some("0.31.0-rc.1"));
    assert!(parse(data, "0.31.0", Channel::Preview, Target::WindowsX64)
        .unwrap()
        .release_version
        .is_none());
    assert!(parse(
        json!([release(
            "0.30.0+later",
            "PeerBrush-0.30.0+later-Windows-x64.zip"
        )]),
        "0.30.0+current",
        Channel::Stable,
        Target::WindowsX64
    )
    .unwrap()
    .release_version
    .is_none());
}

#[test]
fn unavailable_new_platform_never_falls_back_to_an_older_package() {
    let report = parse(
        json!([
            release("0.30.0", "PeerBrush-0.30.0-Windows-x64.zip"),
            release("0.31.0", "PeerBrush-0.31.0-macOS-arm64-apple-signed-r1.zip")
        ]),
        "0.26.0",
        Channel::Stable,
        Target::WindowsX64,
    )
    .unwrap();
    assert_eq!(report.release_version.as_deref(), Some("0.31.0"));
    assert!(report.assets.is_empty());
    let mut missing = release("0.31.0", "PeerBrush-0.31.0-Windows-x64.zip");
    missing["assets"][0]["state"] = json!("new");
    assert!(parse(
        json!([missing]),
        "0.30.0",
        Channel::Stable,
        Target::WindowsX64
    )
    .unwrap()
    .assets
    .is_empty());
    assert!(Target::MacArm64.accepts(
        "PeerBrush-0.31.0-macOS-arm64-apple-signed-r1-dmg-signed-r2.dmg",
        "0.31.0"
    ));
    assert!(Target::LinuxX64.accepts("peerbrush_0.31.0-1_amd64.deb", "0.31.0"));
    assert!(Target::LinuxX64.accepts("peerbrush_0.31.0~rc-2-1_amd64.deb", "0.31.0-rc-2"));
    assert!(!Target::LinuxX64.accepts("peerbrush_0.31.0_amd64.deb", "0.31.0"));
    assert!(!Target::MacArm64.accepts("PeerBrush-0.31.0-macOS-arm64-dmg-signed-r2.dmg", "0.31.0"));
    assert!(!Target::MacArm64.accepts(
        "PeerBrush-0.31.0-macOS-arm64-dmg-candidate-r2.dmg",
        "0.31.0"
    ));
    assert!(!Target::WindowsX64.accepts("PeerBrush-0.31.0-Windows-arm64.zip", "0.31.0"));
}

#[test]
fn unofficial_and_ambiguous_metadata_links_are_rejected() {
    for url in [
        "http://github.com/Deftr0y/PeerBrush/releases/tag/v0.30.0",
        "https://github.com.evil.example/Deftr0y/PeerBrush/releases/tag/v0.30.0",
        "https://attacker@github.com/Deftr0y/PeerBrush/releases/tag/v0.30.0",
        "https://github.com/Deftr0y/PeerBrush/releases/tag/v0.30.0?next=evil",
        "https://github.com/Deftr0y/Other/releases/tag/v0.30.0",
    ] {
        let mut item = release("0.30.0", "PeerBrush-0.30.0-Windows-x64.zip");
        item["html_url"] = json!(url);
        assert!(parse(json!([item]), "0.26.0", Channel::Stable, Target::WindowsX64).is_err());
    }
    let mut item = release("0.30.0", "PeerBrush-0.30.0-Windows-x64.zip");
    item["assets"][0]["browser_download_url"] = json!("https://evil.example/payload.exe");
    assert!(parse(json!([item]), "0.26.0", Channel::Stable, Target::WindowsX64).is_err());
}

#[test]
fn invalid_oversized_and_interrupted_responses_never_claim_update_success() {
    assert!(parse_releases(
        b"not json",
        "0.26.0",
        Channel::Stable,
        Target::WindowsX64,
        1
    )
    .is_err());
    assert!(parse_releases(
        &vec![0; RESPONSE_LIMIT + 1],
        "0.26.0",
        Channel::Stable,
        Target::WindowsX64,
        1
    )
    .is_err());
    struct Offline;
    impl Read for Offline {
        fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
            Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "interrupted network fixture",
            ))
        }
    }
    assert!(read_response(Offline, &|_| {}).is_err());
    assert!(read_response(std::io::repeat(0).take(RESPONSE_LIMIT as u64 + 1), &|_| {}).is_err());
    let mut large = release("0.30.0", "PeerBrush-0.30.0-Windows-x64.zip");
    large["body"] = json!("word ".repeat(5000));
    let report = parse(
        json!([large]),
        "0.26.0",
        Channel::Stable,
        Target::WindowsX64,
    )
    .unwrap();
    assert!(report
        .notes
        .ends_with("Notes truncated; read the full official release page."));
}

#[test]
fn guarded_preferences_survive_restart_and_reject_external_or_future_changes() {
    let root = std::env::temp_dir().join(format!("pb-update-prefs-{}", crate::engine::id()));
    fs::create_dir(&root).unwrap();
    let mut store = Store::load(&root);
    assert!(!store.due(1));
    let preferences = Preferences {
        automatic_checks: true,
        channel: Channel::Preview,
        last_attempt: Some(100),
        ..Default::default()
    };
    store.save(preferences.clone()).unwrap();
    let mut restarted = Store::load(&root);
    assert_eq!(restarted.preferences, preferences);
    assert!(!restarted.due(100 + CHECK_INTERVAL - 1));
    assert!(restarted.due(100 + CHECK_INTERVAL));
    assert!(!restarted.due(99));
    let path = root.join("updates.json");
    fs::write(
        &path,
        br#"{"version":7,"automatic_checks":true,"channel":"stable","last_attempt":null}"#,
    )
    .unwrap();
    assert!(restarted.save(Preferences::default()).is_err());
    let mut protected = Store::load(&root);
    assert!(protected.error.is_some());
    assert!(protected.save(Preferences::default()).is_err());
    assert!(fs::read_to_string(path).unwrap().contains("\"version\":7"));
    fs::remove_dir_all(root).unwrap();
}

fn workspace() -> (crate::server::Shared, crate::workspace::Registry) {
    let mut engine = crate::engine::Engine::new();
    engine.doc = crate::engine::Document::new(8, 8).unwrap();
    let root = Arc::new(Mutex::new(engine));
    let registry = crate::workspace::attach(&root);
    (root, registry)
}

#[test]
fn update_exit_refuses_unsaved_discard_loading_and_source_races() {
    use crate::workspace::lifecycle::{finish_update_exit, update_readiness};
    for depth in [8, 16] {
        let (root, registry) = workspace();
        root.lock().unwrap().doc = crate::engine::Document::new_depth(8, 8, depth).unwrap();
        let reviewed = update_readiness(&registry).unwrap();
        let mut discard = reviewed.clone();
        discard[0].discard = true;
        assert!(finish_update_exit(&registry, &discard).is_err());
        root.lock().unwrap().doc.revision += 1;
        assert!(finish_update_exit(&registry, &reviewed).is_err());
        assert!(update_readiness(&registry).is_err());
        {
            let mut engine = root.lock().unwrap();
            engine.saved_revision = engine.doc.revision;
        }
        assert!(update_readiness(&registry).is_ok());
        assert!(!root.lock().unwrap().closed);
    }
}

#[test]
fn update_exit_serializes_live_file_operations_and_rechecks_project_set() {
    use crate::workspace::lifecycle::{finish_update_exit, update_readiness};
    let (root, registry) = workspace();
    let reviewed = update_readiness(&registry).unwrap();
    let io = crate::server::FileOperation::begin(&root).unwrap();
    let frozen_engine = root.lock().unwrap().clone();
    assert_eq!(
        frozen_engine
            .file_operations
            .load(std::sync::atomic::Ordering::Acquire),
        1
    );
    assert!(update_readiness(&registry).is_err());
    assert!(finish_update_exit(&registry, &reviewed).is_err());
    assert!(!root.lock().unwrap().closed);
    drop(io);
    assert_eq!(
        frozen_engine
            .file_operations
            .load(std::sync::atomic::Ordering::Acquire),
        0
    );
    root.lock().unwrap().loading = Some(crate::loading::Control::default());
    assert!(finish_update_exit(&registry, &reviewed).is_err());
    root.lock().unwrap().loading = None;
    let recovery =
        crate::server::FileOperation::track(registry.lock().unwrap().file_operations.clone());
    assert!(finish_update_exit(&registry, &reviewed).is_err());
    drop(recovery);
    let second = crate::workspace::new_project(&root, 8, 8, 8, None).unwrap();
    assert!(finish_update_exit(&registry, &reviewed).is_err());
    assert!(!second.lock().unwrap().closed);
    let current = update_readiness(&registry).unwrap();
    finish_update_exit(&registry, &current).unwrap();
    assert!(root.lock().unwrap().closed);
    assert!(second.lock().unwrap().closed);
    assert!(crate::server::FileOperation::begin(&root).is_err());
}

#[test]
fn update_exit_protects_external_source_conflicts_without_saving_over_them() {
    use crate::workspace::lifecycle::{finish_update_exit, update_readiness};
    let (root, registry) = workspace();
    let directory =
        std::env::temp_dir().join(format!("pb-update-conflict-{}", crate::engine::id()));
    fs::create_dir(&directory).unwrap();
    let path = directory.join("source.psd");
    crate::server::save(&root, &path).unwrap();
    let reviewed = update_readiness(&registry).unwrap();
    let pixels = root.lock().unwrap().doc.export_png().unwrap();
    fs::write(&path, b"newer external source").unwrap();
    assert!(update_readiness(&registry).is_err());
    assert!(finish_update_exit(&registry, &reviewed).is_err());
    assert_eq!(root.lock().unwrap().doc.export_png().unwrap(), pixels);
    assert!(!root.lock().unwrap().closed);
    assert_eq!(fs::read(path).unwrap(), b"newer external source");
    fs::remove_dir_all(directory).unwrap();
}

#[test]
fn update_exit_waits_for_shared_preferences_and_rejects_mutations_after_closing() {
    use crate::workspace::lifecycle::{finish_update_exit, update_readiness};
    let directory = std::env::temp_dir().join(format!("pb-update-library-{}", crate::engine::id()));
    fs::create_dir(&directory).unwrap();
    let (root, registry) = workspace();
    registry.lock().unwrap().recovery_dir = Some(directory.clone());
    let library = root.lock().unwrap().brush_library.clone();
    *library.lock().unwrap() = crate::brush_library::Library::load(directory.join("brushes.json"));
    let reviewed = update_readiness(&registry).unwrap();
    let held = library.lock().unwrap();
    let worker_root = root.clone();
    let worker = std::thread::spawn(move || {
        crate::server::dispatch(
            &worker_root,
            "brushes",
            &json!({
                "action":"save", "name":"Guarded brush", "category":"Custom", "settings":crate::brush::Settings::default()
            }),
        )
    });
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while registry
        .lock()
        .unwrap()
        .file_operations
        .load(std::sync::atomic::Ordering::Acquire)
        == 0
    {
        assert!(
            std::time::Instant::now() < deadline,
            "Preference operation did not start"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(finish_update_exit(&registry, &reviewed).is_err());
    assert!(!root.lock().unwrap().closed);
    drop(held);
    assert!(worker.join().unwrap().is_ok());
    let bytes = fs::read(directory.join("brushes.json")).unwrap();
    finish_update_exit(&registry, &reviewed).unwrap();
    for (method, action) in [
        ("brushes", "save"),
        ("brushes", "delete"),
        ("filters", "save"),
        ("filters", "rename"),
        ("filters", "delete"),
        ("filters", "import"),
        ("tool_settings", "set"),
        ("exports", "save_preset"),
        ("exports", "delete_preset"),
        ("recovery", "discard"),
    ] {
        let error = crate::server::dispatch(
            &root,
            method,
            &json!({"action":action,"actor":"human","snapshot":"missing"}),
        )
        .unwrap_err();
        assert!(error.contains("exiting"), "{method}/{action}: {error}");
    }
    assert!(crate::server::dispatch(&root, "brushes", &json!({"action":"list"})).is_ok());
    assert!(crate::server::dispatch(&root, "tool_settings", &json!({"action":"get"})).is_ok());
    assert_eq!(fs::read(directory.join("brushes.json")).unwrap(), bytes);
    fs::remove_dir_all(directory).unwrap();
}

#[test]
fn instance_preferences_still_work_after_the_original_project_tab_closes() {
    let (root, _) = workspace();
    let _second = crate::workspace::new_project(&root, 8, 8, 8, None).unwrap();
    let (project, document, revision) = {
        let engine = root.lock().unwrap();
        (
            engine.project_id.clone(),
            engine.doc.id.clone(),
            engine.doc.revision,
        )
    };
    crate::workspace::close(&root, &project, &document, revision, false, "human").unwrap();
    assert!(root.lock().unwrap().closed);
    assert!(crate::server::dispatch(&root, "brushes", &json!({"action":"save","name":"Still shared", "category":"Custom", "settings":crate::brush::Settings::default()})).is_ok());
}
