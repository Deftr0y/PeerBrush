//! Filesystem fixtures exercise preservation/journal logic, not native authority.
use super::*;

type ProfileObserver = Box<dyn FnMut(&Path) -> Result<(), String>>;
thread_local! {
    static PROFILE_OBSERVER: std::cell::RefCell<Option<ProfileObserver>> = const { std::cell::RefCell::new(None) };
}
pub(super) fn after_profile_entry(relative: &Path) -> Result<(), String> {
    let observer = PROFILE_OBSERVER.with(|slot| slot.borrow_mut().take());
    let Some(mut observer) = observer else {
        return Ok(());
    };
    let result = observer(relative);
    PROFILE_OBSERVER.with(|slot| *slot.borrow_mut() = Some(observer));
    result
}
struct WatchProfile;
impl WatchProfile {
    fn new(observer: impl FnMut(&Path) -> Result<(), String> + 'static) -> Self {
        PROFILE_OBSERVER
            .with(|slot| assert!(slot.borrow_mut().replace(Box::new(observer)).is_none()));
        Self
    }
}
impl Drop for WatchProfile {
    fn drop(&mut self) {
        PROFILE_OBSERVER.with(|slot| slot.borrow_mut().take());
    }
}
fn fixture_link_mode(path: &Path, mode: libc::mode_t) {
    let name = CString::new(path.as_os_str().as_bytes()).unwrap();
    assert_eq!(unsafe { libc::lchmod(name.as_ptr(), mode) }, 0);
}
fn fixture_link_alias(source: &Path, target: &Path) {
    let source = CString::new(source.as_os_str().as_bytes()).unwrap();
    let target = CString::new(target.as_os_str().as_bytes()).unwrap();
    assert_eq!(
        unsafe {
            libc::linkat(
                libc::AT_FDCWD,
                source.as_ptr(),
                libc::AT_FDCWD,
                target.as_ptr(),
                0,
            )
        },
        0
    );
}

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().canonicalize().unwrap().join(format!(
            "peerbrush-update-checkpoint-{}",
            uuid::Uuid::new_v4()
        ));
        fs::DirBuilder::new().mode(0o700).create(&root).unwrap();
        Self(root)
    }
    fn locations(&self) -> Locations {
        Locations {
            config: self.0.join("config"),
            data: self.0.join("data"),
            cache: self.0.join("cache"),
        }
    }
    fn profile(&self) -> crate::settings::Profile {
        let legacy = self.0.join("legacy");
        fs::create_dir(&legacy).unwrap();
        fs::write(
            legacy.join("tool-settings.json"),
            br#"{"version":1,"tools":{}}"#,
        )
        .unwrap();
        crate::settings::Profile::open(self.locations(), &legacy).unwrap()
    }
    fn attempt(&self, profile: &UpdateProfile) -> PathBuf {
        let locations = self.locations();
        private(&locations.data, true).unwrap();
        let root = checkpoints(&locations);
        private(&root, true).unwrap();
        let attempt = root.join(uuid::Uuid::new_v4().to_string());
        fs::DirBuilder::new().mode(0o700).create(&attempt).unwrap();
        let source = self.app();
        let mut app = Snapshot::default();
        app.copy(
            &source,
            Path::new(""),
            &attempt.join("PeerBrush.app"),
            true,
            MAX_APP_BYTES,
            &AtomicBool::new(false),
        )
        .unwrap();
        let preferences =
            copy_profile(profile, &attempt.join("profile"), &AtomicBool::new(false)).unwrap();
        let manifest = Manifest {
            schema: 1,
            target: source,
            publisher_team_id: "ABCD123456".into(),
            build_record: crate::build_info::json().into(),
            app: app.entries,
            profile: preferences.entries,
        };
        // This fixture does not replace production's independent native checks.
        write_new(
            &attempt.join("checkpoint.json"),
            &serde_json::to_vec(&manifest).unwrap(),
        )
        .unwrap();
        attempt
    }
    fn app(&self) -> PathBuf {
        let app = self.0.join("original.app");
        fs::create_dir(&app).unwrap();
        fs::create_dir_all(app.join("Contents/Frameworks/Sparkle.framework/Versions/B")).unwrap();
        fs::create_dir_all(app.join("Contents/MacOS")).unwrap();
        fs::write(app.join("Contents/MacOS/peerbrush"), [0, 1, 2, 254, 255]).unwrap();
        fs::set_permissions(
            app.join("Contents/MacOS/peerbrush"),
            fs::Permissions::from_mode(0o755),
        )
        .unwrap();
        fs::write(
            app.join("Contents/Frameworks/Sparkle.framework/Versions/B/Sparkle"),
            b"fixture only",
        )
        .unwrap();
        std::os::unix::fs::symlink(
            "B",
            app.join("Contents/Frameworks/Sparkle.framework/Versions/Current"),
        )
        .unwrap();
        std::os::unix::fs::symlink(
            "Versions/Current/Sparkle",
            app.join("Contents/Frameworks/Sparkle.framework/Sparkle"),
        )
        .unwrap();
        for name in [
            "",
            "Contents",
            "Contents/MacOS",
            "Contents/Frameworks",
            "Contents/Frameworks/Sparkle.framework",
            "Contents/Frameworks/Sparkle.framework/Versions",
            "Contents/Frameworks/Sparkle.framework/Versions/B",
        ] {
            fs::set_permissions(app.join(name), fs::Permissions::from_mode(0o755)).unwrap();
        }
        fs::set_permissions(
            app.join("Contents/Frameworks/Sparkle.framework/Versions/B/Sparkle"),
            fs::Permissions::from_mode(0o755),
        )
        .unwrap();
        for name in [
            "Contents/Frameworks/Sparkle.framework/Versions/Current",
            "Contents/Frameworks/Sparkle.framework/Sparkle",
        ] {
            fixture_link_mode(&app.join(name), 0o777);
        }
        fs::create_dir_all(app.join("Contents/Resources/readonly")).unwrap();
        fs::set_permissions(
            app.join("Contents/Resources"),
            fs::Permissions::from_mode(0o755),
        )
        .unwrap();
        fs::set_permissions(
            app.join("Contents/Resources/readonly"),
            fs::Permissions::from_mode(0o555),
        )
        .unwrap();
        app
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn actual_profile_copy_preserves_current_settings_and_old_migration_backup_without_runtime_files() {
    let f = Fixture::new();
    let profile = f.profile();
    let source = profile.locations.profile();
    let old = fs::read(source.join("tool-settings.json")).unwrap();
    let mut tools = crate::tool_settings::Store::load(source.join("tool-settings.json"));
    let mut brush = tools.get("brush").unwrap().profile;
    brush.settings.radius = 43.;
    tools.set("brush", brush, "human", None).unwrap();
    let current = fs::read(source.join("tool-settings.json")).unwrap();
    assert_ne!(old, current);
    for name in [
        "connection.json",
        "private.json",
        "recovery.psd",
        "build.exe",
    ] {
        fs::write(source.join(name), b"private original retained").unwrap();
        fs::write(
            source.join("migration-backup").join(name),
            b"private original retained",
        )
        .unwrap();
    }
    let lease = profile.update_profile();
    let target = f.0.join("preserved-profile");
    let snapshot = copy_profile(&lease, &target, &AtomicBool::new(false)).unwrap();
    check_copy(&target, &snapshot.entries, &AtomicBool::new(false)).unwrap();
    assert_eq!(
        fs::read(target.join("tool-settings.json")).unwrap(),
        current
    );
    assert_eq!(
        fs::read(target.join("migration-backup/tool-settings.json")).unwrap(),
        old
    );
    for name in [
        "connection.json",
        "private.json",
        "recovery.psd",
        "build.exe",
    ] {
        assert!(!target.join(name).exists());
        assert!(!target.join("migration-backup").join(name).exists());
        assert_eq!(
            fs::read(source.join(name)).unwrap(),
            b"private original retained"
        );
    }
    drop(profile);
    assert!(crate::settings::Profile::open(f.locations(), &f.0.join("legacy")).is_err());
    fs::write(source.join("updates.json"), b"changed after snapshot").unwrap();
    assert!(snapshot.recheck().is_err());
    drop(lease);
}

#[test]
fn full_byte_copy_preserves_executable_modes_and_only_exact_pinned_framework_links() {
    let f = Fixture::new();
    let source = f.app();
    let target = f.0.join("preserved.app");
    let mut snapshot = Snapshot::default();
    snapshot
        .copy(
            &source,
            Path::new(""),
            &target,
            true,
            MAX_APP_BYTES,
            &AtomicBool::new(false),
        )
        .unwrap();
    snapshot.recheck().unwrap();
    let copied = check_copy(&target, &snapshot.entries, &AtomicBool::new(false)).unwrap();
    copied.recheck().unwrap();
    assert_eq!(
        fs::read(target.join("Contents/MacOS/peerbrush")).unwrap(),
        [0, 1, 2, 254, 255]
    );
    assert_eq!(
        fs::metadata(target.join("Contents/MacOS/peerbrush"))
            .unwrap()
            .mode()
            & 0o7777,
        0o755
    );
    assert_eq!(
        fs::read_link(target.join("Contents/Frameworks/Sparkle.framework/Sparkle")).unwrap(),
        Path::new("Versions/Current/Sparkle")
    );
    fs::write(source.join("Contents/MacOS/peerbrush"), b"changed").unwrap();
    assert!(snapshot.recheck().is_err());
    assert_eq!(
        fs::read(target.join("Contents/MacOS/peerbrush")).unwrap(),
        [0, 1, 2, 254, 255]
    );
}

#[test]
fn copy_and_independent_parity_refuse_aliases_extra_members_changed_bytes_and_modes() {
    for damage in [
        "unknown-link",
        "escaping-link",
        "hardlink",
        "changed-copy",
        "extra-copy",
        "mode-copy",
    ] {
        let f = Fixture::new();
        let source = f.app();
        let target = f.0.join("preserved.app");
        match damage {
            "unknown-link" => {
                let path = source.join("Contents/unknown");
                std::os::unix::fs::symlink("MacOS/peerbrush", &path).unwrap();
                fixture_link_mode(&path, 0o777);
            }
            "escaping-link" => {
                let path = source.join("Contents/Frameworks/Sparkle.framework/Sparkle");
                fs::remove_file(&path).unwrap();
                std::os::unix::fs::symlink("/etc/passwd", &path).unwrap();
                fixture_link_mode(&path, 0o777);
            }
            "hardlink" => fs::hard_link(
                source.join("Contents/MacOS/peerbrush"),
                source.join("Contents/duplicate"),
            )
            .unwrap(),
            _ => {}
        }
        let mut snapshot = Snapshot::default();
        let result = snapshot.copy(
            &source,
            Path::new(""),
            &target,
            true,
            MAX_APP_BYTES,
            &AtomicBool::new(false),
        );
        if matches!(damage, "unknown-link" | "escaping-link" | "hardlink") {
            assert!(result.is_err(), "{damage}");
            continue;
        }
        result.unwrap();
        match damage {
            "changed-copy" => {
                fs::write(target.join("Contents/MacOS/peerbrush"), [0, 1, 2, 254, 253]).unwrap()
            }
            "extra-copy" => fs::write(target.join("Contents/private"), b"not in source").unwrap(),
            "mode-copy" => fs::set_permissions(
                target.join("Contents/MacOS/peerbrush"),
                fs::Permissions::from_mode(0o777),
            )
            .unwrap(),
            _ => unreachable!(),
        }
        assert!(
            check_copy(&target, &snapshot.entries, &AtomicBool::new(false)).is_err(),
            "{damage}"
        );
    }
}

#[test]
fn cancellation_and_byte_budget_never_mutate_or_remove_original_sources() {
    for cancelled in [false, true] {
        let f = Fixture::new();
        let source = f.app();
        let target = f.0.join("preserved.app");
        let mut snapshot = Snapshot::default();
        assert!(snapshot
            .copy(
                &source,
                Path::new(""),
                &target,
                true,
                1,
                &AtomicBool::new(cancelled)
            )
            .is_err());
        assert_eq!(
            fs::read(source.join("Contents/MacOS/peerbrush")).unwrap(),
            [0, 1, 2, 254, 255]
        );
        assert_eq!(
            fs::read_link(source.join("Contents/Frameworks/Sparkle.framework/Sparkle")).unwrap(),
            Path::new("Versions/Current/Sparkle")
        );
        if cancelled {
            assert!(!target.exists());
        }
    }
}

#[test]
fn real_profile_startup_preserves_sources_and_refuses_every_unknown_or_misbound_native_attempt() {
    let f = Fixture::new();
    let profile = f.profile();
    let path = profile.locations.profile();
    let original = fs::read(path.join("tool-settings.json")).unwrap();
    let manifest_original = fs::read(path.join("profile.json")).unwrap();
    let attempt = f.attempt(&profile.update_profile());
    let manifest = read_private(&attempt.join("checkpoint.json"), 1024 * 1024).unwrap();
    startup_guard(&f.locations()).unwrap(); // No intent: no native call was permitted.
    record_intent(&attempt, &manifest).unwrap();
    assert!(record_intent(&attempt, &manifest).is_err()); // No overwrite/reuse.
    drop(profile);
    assert!(crate::settings::Profile::open(f.locations(), &f.0.join("legacy")).is_err());
    assert_eq!(fs::read(path.join("tool-settings.json")).unwrap(), original);
    assert_eq!(
        fs::read(path.join("profile.json")).unwrap(),
        manifest_original
    );
    assert!(record_unstarted(&attempt, b"foreign checkpoint").is_err());
    assert!(startup_guard(&f.locations()).is_err());
    // The production caller invokes this only for actual startUpdater:NO.
    // This filesystem fixture does not claim that native result was executed.
    record_unstarted(&attempt, &manifest).unwrap();
    startup_guard(&f.locations()).unwrap();
    assert!(record_unstarted(&attempt, &manifest).is_err());
    let reopened = crate::settings::Profile::open(f.locations(), &f.0.join("legacy")).unwrap();
    assert_eq!(fs::read(path.join("tool-settings.json")).unwrap(), original);
    drop(reopened);
    let backup = attempt.join("profile/tool-settings.json");
    let retained = fs::read(&backup).unwrap();
    fs::remove_file(&backup).unwrap();
    assert!(startup_guard(&f.locations()).is_err());
    assert_eq!(fs::read(path.join("tool-settings.json")).unwrap(), original);
    fs::write(&backup, &retained).unwrap();
    startup_guard(&f.locations()).unwrap();
    fs::write(attempt.join("checkpoint.json"), b"modified manifest").unwrap();
    assert!(startup_guard(&f.locations()).is_err());
}

#[test]
fn profile_backup_alias_and_nonprivate_journal_remain_held_without_copying_foreign_bytes() {
    let f = Fixture::new();
    let profile = f.profile();
    let attempt = f.attempt(&profile.update_profile());
    let backup = profile.locations.profile().join("migration-backup");
    fs::remove_dir_all(&backup).unwrap();
    std::os::unix::fs::symlink(&f.0, &backup).unwrap();
    assert!(copy_profile(
        &profile.update_profile(),
        &f.0.join("copy"),
        &AtomicBool::new(false)
    )
    .is_err());
    assert!(!f
        .0
        .join("copy/migration-backup/tool-settings.json")
        .exists());
    let manifest = read_private(&attempt.join("checkpoint.json"), 1024 * 1024).unwrap();
    record_intent(&attempt, &manifest).unwrap();
    record_unstarted(&attempt, &manifest).unwrap();
    fs::set_permissions(
        attempt.join("native-not-started.json"),
        fs::Permissions::from_mode(0o644),
    )
    .unwrap();
    assert!(startup_guard(&f.locations()).is_err());
}

#[test]
fn actual_publication_reads_queued_profile_bytes_and_absence_without_authorizing_native_start() {
    for queued in [
        "unchanged",
        "same-metadata-content",
        "missing-became-present",
    ] {
        let f = Fixture::new();
        let profile = f.profile();
        let root = profile.locations.profile();
        let file = root.join("tool-settings.json");
        let mut tools = crate::tool_settings::Store::load(file.clone());
        let mut brush = tools.get("brush").unwrap().profile;
        brush.settings.radius = 43.;
        tools.set("brush", brush, "human", None).unwrap();
        let mut value: serde_json::Value =
            serde_json::from_slice(&fs::read(&file).unwrap()).unwrap();
        let original = serde_json::to_vec_pretty(&value).unwrap();
        fs::write(&file, &original).unwrap();
        let mut captured = copy_profile(
            &profile.update_profile(),
            &f.0.join("copy"),
            &AtomicBool::new(false),
        )
        .unwrap();
        let publication = f.0.join("publication");
        fs::DirBuilder::new()
            .mode(0o700)
            .create(&publication)
            .unwrap();
        let source = std::sync::Arc::new(std::sync::Mutex::new(crate::engine::Engine::new()));
        let workspace = crate::workspace::attach(&source);
        let reviewed = crate::workspace::lifecycle::update_readiness(&workspace).unwrap();
        if queued == "same-metadata-content" {
            value["tools"]["brush"]["settings"]["radius"] = serde_json::json!(44.);
            let changed = serde_json::to_vec_pretty(&value).unwrap();
            assert_eq!(changed.len(), original.len());
            assert_ne!(changed, original);
            fs::write(&file, &changed).unwrap();
            assert!(crate::tool_settings::Store::load(file.clone())
                .error
                .is_none());
            let now = Stamp::read(&file).unwrap().unwrap();
            let witness = captured.stamps.get_mut(&file).unwrap().as_mut().unwrap();
            assert_eq!(
                (
                    witness.device,
                    witness.inode,
                    witness.bytes,
                    witness.mode,
                    witness.links,
                    witness.uid,
                    witness.gid
                ),
                (now.device, now.inode, now.bytes, now.mode, now.links, now.uid, now.gid)
            );
            // Reproduce identical queued metadata witnesses on any test volume.
            // HFS+ can supply this collision without this fixture normalization.
            // The worker's authenticated byte digest deliberately remains old.
            witness.modified = now.modified;
            witness.changed = now.changed;
            captured.recheck().unwrap();
        } else if queued == "missing-became-present" {
            let path = root.join("updates.json");
            assert!(captured.stamps.get(&path).unwrap().is_none());
            fs::write(
                path,
                serde_json::to_vec(&crate::updates::Preferences::default()).unwrap(),
            )
            .unwrap();
        }
        let expected_current = fs::read(&file).unwrap();
        let result =
            crate::workspace::lifecycle::publish_update_checkpoint(&workspace, &reviewed, || {
                publish_profile_checkpoint(
                    &captured,
                    &root,
                    &publication,
                    b"private checkpoint-only fixture",
                )
            });
        if queued == "unchanged" {
            result.unwrap();
            assert!(publication.join("checkpoint.json").is_file());
            assert!(require_captured_handoff().is_err());
        } else {
            assert!(result.is_err(), "{queued}");
            assert!(!publication.join("checkpoint.json").exists());
        }
        assert!(!publication.join("native-start-intent.json").exists());
        assert!(!publication.join("native-not-started.json").exists());
        assert_eq!(fs::read(&file).unwrap(), expected_current);
        assert!(!source.lock().unwrap().closed);
        assert!(!workspace.lock().unwrap().exiting);
    }
}

#[test]
fn deep_and_long_trees_are_refused_by_writer_and_consumer_with_all_sources_retained() {
    let f = Fixture::new();
    let profile = f.profile();
    let preferences = fs::read(profile.locations.profile().join("tool-settings.json")).unwrap();
    let mut engine = crate::engine::Engine::new();
    engine.doc = crate::engine::Document::new_depth(4, 3, 16).unwrap();
    engine.doc.layers[0]
        .pixels
        .set16(0, 0, [1001, 3003, 5007, 0]);
    let source = std::sync::Arc::new(std::sync::Mutex::new(engine));
    let source_path = f.0.join("keep.psd");
    crate::server::save(&source, &source_path).unwrap();
    let document = serde_json::to_value(&source.lock().unwrap().doc).unwrap();
    let document_bytes = fs::read(&source_path).unwrap();
    let input = f.0.join("deep-input");
    fs::create_dir(&input).unwrap();
    let mut expected = BTreeMap::new();
    expected.insert(PathBuf::new(), Entry::Directory { mode: 0o755 });
    let mut relative = PathBuf::new();
    for _ in 0..=MAX_DEPTH {
        relative.push("d");
        fs::create_dir(input.join(&relative)).unwrap();
        expected.insert(relative.clone(), Entry::Directory { mode: 0o755 });
    }
    let leaf = input.join(&relative).join("untouched");
    fs::write(&leaf, b"original deep bytes").unwrap();
    let mut snapshot = Snapshot::default();
    assert_eq!(
        snapshot
            .copy(
                &input,
                Path::new(""),
                &f.0.join("copy"),
                true,
                MAX_APP_BYTES,
                &AtomicBool::new(false)
            )
            .unwrap_err(),
        PATH_BUDGET
    );
    assert_eq!(
        check_copy(&input, &expected, &AtomicBool::new(false))
            .err()
            .unwrap(),
        PATH_BUDGET
    );
    let retained_profile = copy_profile(
        &profile.update_profile(),
        &f.0.join("profile-copy"),
        &AtomicBool::new(false),
    )
    .unwrap();
    let mut manifest = Manifest {
        schema: 1,
        target: input.clone(),
        publisher_team_id: "ABCD123456".into(),
        build_record: crate::build_info::json().into(),
        app: expected.clone(),
        profile: retained_profile.entries,
    };
    assert_eq!(validate_manifest(&manifest).unwrap_err(), PATH_BUDGET);
    let long = PathBuf::from("x".repeat(MAX_PATH_BYTES + 1));
    assert_eq!(
        Snapshot::default()
            .copy(
                &input,
                &long,
                &f.0.join("long-copy"),
                true,
                MAX_APP_BYTES,
                &AtomicBool::new(false)
            )
            .unwrap_err(),
        PATH_BUDGET
    );
    expected.clear();
    expected.insert(PathBuf::new(), Entry::Directory { mode: 0o755 });
    expected.insert(long, Entry::Directory { mode: 0o755 });
    assert_eq!(
        check_copy(&input, &expected, &AtomicBool::new(false))
            .err()
            .unwrap(),
        PATH_BUDGET
    );
    manifest.app = expected;
    assert_eq!(validate_manifest(&manifest).unwrap_err(), PATH_BUDGET);
    assert_eq!(fs::read(&leaf).unwrap(), b"original deep bytes");
    assert_eq!(
        fs::read(profile.locations.profile().join("tool-settings.json")).unwrap(),
        preferences
    );
    assert_eq!(
        serde_json::to_value(&source.lock().unwrap().doc).unwrap(),
        document
    );
    assert_eq!(fs::read(&source_path).unwrap(), document_bytes);
    assert!(!source.lock().unwrap().closed);
    assert!(source.lock().unwrap().file_ownership.is_some());
    assert!(crate::file_ownership::Ownership::open(&source_path, "unrelated-project").is_err());
}

#[test]
fn copied_modes_under_restrictive_masks_are_preserved_in_isolated_process() {
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "sparkle::macos::checkpoint::tests::copy_modes_isolated_child",
            "--ignored",
            "--nocapture",
            "--test-threads=1",
            "--color=never",
        ])
        .env_clear()
        .env("TMPDIR", std::env::temp_dir())
        .env("PEERBRUSH_SPARKLE_COPY_MODE_CHILD", "1")
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success() && stdout.contains("1 passed; 0 failed; 0 ignored;"),
        "isolated copy fixture did not pass: {stdout} {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
#[ignore = "Internal isolated copy-mode worker; acceptance runs through the parent test"]
fn copy_modes_isolated_child() {
    assert_eq!(
        std::env::var("PEERBRUSH_SPARKLE_COPY_MODE_CHILD")
            .ok()
            .as_deref(),
        Some("1")
    );
    // Only this separately launched single-test process changes its umask.
    for mask in [0o022, 0o077] {
        let old_mask = unsafe { libc::umask(mask) };
        for damage in [
            "none",
            "source-link-mode",
            "source-link-alias",
            "copy-link-mode",
            "copy-link-alias",
        ] {
            let f = Fixture::new();
            let profile = f.profile();
            let profile_bytes =
                fs::read(profile.locations.profile().join("tool-settings.json")).unwrap();
            let attempt = f.attempt(&profile.update_profile());
            let manifest_bytes =
                read_private(&attempt.join("checkpoint.json"), 1024 * 1024).unwrap();
            let manifest: Manifest = serde_json::from_slice(&manifest_bytes).unwrap();
            let source = f.0.join("original.app");
            let copy = attempt.join("PeerBrush.app");
            let link_name = "Contents/Frameworks/Sparkle.framework/Sparkle";
            let original = check_copy(&source, &manifest.app, &AtomicBool::new(false)).unwrap();
            check_copy(&copy, &manifest.app, &AtomicBool::new(false)).unwrap();
            assert_eq!(
                fs::metadata(copy.join("Contents")).unwrap().mode() & 0o7777,
                0o755
            );
            assert_eq!(
                fs::metadata(copy.join("Contents/Resources/readonly"))
                    .unwrap()
                    .mode()
                    & 0o7777,
                0o555
            );
            for name in [
                "Contents/MacOS/peerbrush",
                "Contents/Frameworks/Sparkle.framework/Versions/B/Sparkle",
            ] {
                assert_eq!(
                    fs::read(source.join(name)).unwrap(),
                    fs::read(copy.join(name)).unwrap()
                );
                assert_eq!(
                    fs::metadata(copy.join(name)).unwrap().mode() & 0o7777,
                    0o755
                );
            }
            record_intent(&attempt, &manifest_bytes).unwrap();
            // Filesystem-only outcome fixture, never evidence of an actual SDK NO.
            record_unstarted(&attempt, &manifest_bytes).unwrap();
            startup_guard(&f.locations()).unwrap();
            match damage {
                "source-link-mode" => fixture_link_mode(&source.join(link_name), 0o755),
                "source-link-alias" => {
                    fixture_link_alias(&source.join(link_name), &f.0.join("source-link-alias"))
                }
                "copy-link-mode" => fixture_link_mode(&copy.join(link_name), 0o755),
                "copy-link-alias" => {
                    fixture_link_alias(&copy.join(link_name), &f.0.join("copy-link-alias"))
                }
                _ => {}
            }
            if damage.starts_with("source-") {
                let before = Stamp::read(&source.join(link_name)).unwrap();
                let mut rejected = Snapshot::default();
                assert!(
                    rejected
                        .copy(
                            &source,
                            Path::new(""),
                            &f.0.join("refused.app"),
                            true,
                            MAX_APP_BYTES,
                            &AtomicBool::new(false)
                        )
                        .is_err(),
                    "{damage}"
                );
                assert!(Stamp::read(&source.join(link_name)).unwrap() == before);
                assert_eq!(
                    before.unwrap().links,
                    if damage.ends_with("alias") { 2 } else { 1 }
                );
                assert_eq!(
                    fs::metadata(
                        source.join("Contents/Frameworks/Sparkle.framework/Versions/B/Sparkle")
                    )
                    .unwrap()
                    .nlink(),
                    1
                );
            } else {
                original.recheck().unwrap();
                if damage != "none" {
                    assert!(
                        check_copy(&copy, &manifest.app, &AtomicBool::new(false)).is_err(),
                        "{damage}"
                    );
                    assert!(startup_guard(&f.locations()).is_err(), "{damage}");
                }
            }
            for (path, stamp) in &original.stamps {
                if path != &source.join(link_name) {
                    assert!(
                        Stamp::read(path).unwrap() == *stamp,
                        "original member changed: {}",
                        path.display()
                    );
                }
            }
            assert_eq!(
                fs::read_link(source.join(link_name)).unwrap(),
                Path::new("Versions/Current/Sparkle")
            );
            assert_eq!(
                fs::read(source.join("Contents/MacOS/peerbrush")).unwrap(),
                [0, 1, 2, 254, 255]
            );
            assert_eq!(
                fs::read(source.join("Contents/Frameworks/Sparkle.framework/Versions/B/Sparkle"))
                    .unwrap(),
                b"fixture only"
            );
            assert_eq!(
                fs::metadata(source.join("Contents/MacOS/peerbrush"))
                    .unwrap()
                    .mode()
                    & 0o7777,
                0o755
            );
            assert_eq!(
                fs::metadata(
                    source.join("Contents/Frameworks/Sparkle.framework/Versions/B/Sparkle")
                )
                .unwrap()
                .mode()
                    & 0o7777,
                0o755
            );
            assert_eq!(
                fs::read(profile.locations.profile().join("tool-settings.json")).unwrap(),
                profile_bytes
            );
        }
        unsafe {
            libc::umask(old_mask);
        }
    }
}

#[test]
fn final_profile_recheck_refuses_between_file_replacement_and_creation_without_publication() {
    use serde_json::json;
    use std::sync::{Arc, Mutex};
    for depth in [8, 16] {
        for mutation in ["replace-present", "create-absent"] {
            let f = Fixture::new();
            let profile = f.profile();
            let root = profile.locations.profile();
            let captured = copy_profile(
                &profile.update_profile(),
                &f.0.join("copy"),
                &AtomicBool::new(false),
            )
            .unwrap();
            let retained: Vec<_> = crate::settings::update_files()
                .into_iter()
                .map(|name| {
                    let bytes = fs::read(root.join(&name)).ok();
                    (name, bytes)
                })
                .collect();
            let publication = f.0.join("publication");
            fs::DirBuilder::new()
                .mode(0o700)
                .create(&publication)
                .unwrap();
            let mut engine = crate::engine::Engine::new();
            engine.doc = crate::engine::Document::new_depth(4, 3, depth).unwrap();
            engine.doc.layers[0]
                .pixels
                .set16(0, 0, [1001, 3003, 5007, 0]);
            let layer = engine.doc.layers[0].id.clone();
            engine
                .edit(
                    "human",
                    &[json!({"op":"fill","layer":layer,"rect":[1,1,2,2],"color":[41,83,127,173]})],
                    None,
                    None,
                    "Keep undo",
                )
                .unwrap();
            engine
                .edit(
                    "human",
                    &[json!({"op":"layer.update","layer":layer,"name":"Keep redo"})],
                    None,
                    None,
                    "Keep redo",
                )
                .unwrap();
            engine.undo("human").unwrap();
            engine
                .reserve(
                    "human",
                    "Keep ownership",
                    vec![crate::engine::Scope::layer(&layer)],
                )
                .unwrap();
            let source = Arc::new(Mutex::new(engine));
            let workspace = crate::workspace::attach(&source);
            let source_path = f.0.join("keep.psd");
            crate::server::save(&source, &source_path).unwrap();
            let state = || {
                let e = source.lock().unwrap();
                let history = |items: &[crate::engine::History]| {
                    items.iter().map(|h| json!({"before":h.before,"after":h.after,"actor":h.actor,"label":h.label,"task":h.task,"scopes":h.scopes,"gesture":h.gesture,"reverted":h.reverted})).collect::<Vec<_>>()
                };
                assert!(!e.undo.is_empty() && !e.redo.is_empty() && !e.leases.is_empty());
                json!({"doc":e.doc,"undo":history(&e.undo),"redo":history(&e.redo),"leases":e.leases,"saved":e.saved_revision,"path":e.path,"closed":e.closed,"bytes":fs::read(&source_path).unwrap()})
            };
            let expected = state();
            let owner = source.lock().unwrap().file_ownership.clone().unwrap();
            let version = source.lock().unwrap().file_version;
            let reviewed = crate::workspace::lifecycle::update_readiness(&workspace).unwrap();
            let fired = std::rc::Rc::new(std::cell::Cell::new(false));
            let observed = fired.clone();
            let changed_root = root.clone();
            let _watch = WatchProfile::new(move |relative| {
                if relative == Path::new("migration-backup/tool-settings.json") {
                    assert!(!observed.replace(true));
                    if mutation == "replace-present" {
                        let mut tools = crate::tool_settings::Store::load(
                            changed_root.join("tool-settings.json"),
                        );
                        let mut brush = tools.get("brush").unwrap().profile;
                        brush.settings.radius = 43.;
                        tools.set("brush", brush, "human", None)?;
                    } else {
                        crate::brush_library::Library::load(changed_root.join("brushes.json"))
                            .save(
                                None,
                                "External",
                                "Custom",
                                crate::brush::Settings::default(),
                            )?;
                    }
                }
                Ok(())
            });
            let result = crate::workspace::lifecycle::publish_update_checkpoint(
                &workspace,
                &reviewed,
                || {
                    captured.recheck()?;
                    publish_profile_checkpoint(
                        &captured,
                        &root,
                        &publication,
                        b"checkpoint-only fixture",
                    )
                },
            );
            assert!(fired.get());
            assert!(result.is_err(), "{mutation}");
            assert!(!publication.join("checkpoint.json").exists());
            assert!(!publication.join("native-start-intent.json").exists());
            assert!(!publication.join("native-not-started.json").exists());
            assert!(require_captured_handoff().is_err());
            let changed = Path::new(if mutation == "replace-present" {
                "tool-settings.json"
            } else {
                "brushes.json"
            });
            for (name, old) in retained {
                assert_eq!(fs::read(f.0.join("copy").join(&name)).ok(), old);
                let current = fs::read(root.join(&name)).ok();
                if name == changed {
                    assert_ne!(current, old);
                    assert!(current.is_some());
                } else {
                    assert_eq!(current, old);
                }
            }
            assert_eq!(state(), expected);
            let e = source.lock().unwrap();
            assert_eq!(e.file_version, version);
            assert!(Arc::ptr_eq(e.file_ownership.as_ref().unwrap(), &owner));
            assert!(
                crate::file_ownership::Ownership::open(&source_path, "unrelated-project").is_err()
            );
            assert!(!workspace.lock().unwrap().exiting);
        }
    }
}

#[test]
fn inherited_real_profile_lock_survives_parent_lease_drop_and_refuses_independent_descriptor() {
    let f = Fixture::new();
    let profile = f.profile();
    let lease = profile.update_profile();
    let source = profile.locations.profile().join("tool-settings.json");
    let bytes = fs::read(&source).unwrap();
    let independent = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(profile.locations.config.join("settings.lock"))
        .unwrap();
    assert!(UpdateProfile::from_inherited(f.locations(), independent).is_err());
    let inherited =
        UpdateProfile::from_inherited(f.locations(), lease.inherited_lock().unwrap()).unwrap();
    drop(lease);
    drop(profile);
    assert!(crate::settings::Profile::open(f.locations(), &f.0.join("legacy")).is_err());
    assert_eq!(fs::read(&source).unwrap(), bytes);
    drop(inherited);
    let reopened = crate::settings::Profile::open(f.locations(), &f.0.join("legacy")).unwrap();
    assert_eq!(fs::read(&source).unwrap(), bytes);
    drop(reopened);
}

#[test]
fn authenticated_capture_consumer_uses_exact_derived_roles_and_refuses_damage_without_mutation() {
    for damage in [
        "none",
        "foreign-parent",
        "wrong-digest",
        "armed",
        "changed-copy",
        "native-refusal",
    ] {
        let f = Fixture::new();
        let profile = f.profile();
        let lease = profile.update_profile();
        let profile_bytes =
            fs::read(profile.locations.profile().join("tool-settings.json")).unwrap();
        let attempt = f.attempt(&lease);
        let copy = attempt.join("PeerBrush.app");
        let original = f.0.join("original.app");
        let bytes = read_private(&attempt.join("checkpoint.json"), 1024 * 1024).unwrap();
        write_new(&attempt.join("capture.json"), &bytes).unwrap();
        let name = attempt.file_name().unwrap().to_str().unwrap();
        assert_eq!(
            owner_copy(&copy.join("Contents/MacOS/peerbrush"), &f.locations(), name).unwrap(),
            copy
        );
        assert!(owner_copy(
            &original.join("Contents/MacOS/peerbrush"),
            &f.locations(),
            name
        )
        .is_err());
        assert!(owner_copy(
            &copy.join("Contents/MacOS/peerbrush"),
            &f.locations(),
            "../foreign"
        )
        .is_err());
        let mut parent_exe = original.join("Contents/MacOS/peerbrush");
        let mut expected = digest(&bytes);
        match damage {
            "foreign-parent" => parent_exe = f.0.join("foreign/Contents/MacOS/peerbrush"),
            "wrong-digest" => expected = "f".repeat(64),
            "armed" => record_intent(&attempt, &bytes).unwrap(),
            "changed-copy" => fs::write(copy.join("Contents/MacOS/peerbrush"), b"changed").unwrap(),
            _ => {}
        }
        // Explicit fake native boundary; it supplies no signature authority.
        let configuration = ApprovedCheck {
            feed_url: crate::sparkle::PROPOSED_FEED_URL.into(),
            public_ed_key: "fixture only".into(),
            publisher_team_id: "ABCD123456".into(),
            objects: BTreeMap::new(),
        };
        let mut roles = Vec::new();
        let result = inspect_owner_copy_with(
            &copy,
            &lease,
            &parent_exe,
            &expected,
            &configuration,
            &AtomicBool::new(false),
            |app| {
                roles.push(app);
                if damage == "native-refusal" {
                    Err("native refusal fixture".into())
                } else {
                    Ok(())
                }
            },
        );
        assert_eq!(result.is_ok(), damage == "none", "{damage}");
        if damage == "none" {
            assert_eq!(roles, vec![original.clone(), copy.clone()]);
        }
        if matches!(damage, "foreign-parent" | "wrong-digest" | "armed") {
            assert!(roles.is_empty());
        }
        assert_eq!(
            fs::read(original.join("Contents/MacOS/peerbrush")).unwrap(),
            [0, 1, 2, 254, 255]
        );
        assert_eq!(
            fs::read(profile.locations.profile().join("tool-settings.json")).unwrap(),
            profile_bytes
        );
        assert_eq!(
            read_private(&attempt.join("capture.json"), 1024 * 1024).unwrap(),
            bytes
        );
        assert!(!attempt.join("native-not-started.json").exists());
        assert!(require_captured_handoff().is_err());
    }
}
