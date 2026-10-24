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
type CurrentRecordObserver = Box<dyn FnOnce() -> Result<(), String>>;
thread_local! {
    static CURRENT_RECORD_OBSERVER: std::cell::RefCell<Option<CurrentRecordObserver>> = const { std::cell::RefCell::new(None) };
}
pub(super) fn after_current_profile_record() -> Result<(), String> {
    let observer = CURRENT_RECORD_OBSERVER.with(|slot| slot.borrow_mut().take());
    observer.map_or(Ok(()), |observer| observer())
}
struct WatchCurrentRecord;
impl WatchCurrentRecord {
    fn new(observer: impl FnOnce() -> Result<(), String> + 'static) -> Self {
        CURRENT_RECORD_OBSERVER
            .with(|slot| assert!(slot.borrow_mut().replace(Box::new(observer)).is_none()));
        Self
    }
}
impl Drop for WatchCurrentRecord {
    fn drop(&mut self) {
        CURRENT_RECORD_OBSERVER.with(|slot| slot.borrow_mut().take());
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
            crate::workspace::lifecycle::publish_update_checkpoint(&workspace, &reviewed, |_| {
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
                |_| {
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
        "changed-original",
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
            "changed-original" => fs::write(
                original.join("Contents/MacOS/peerbrush"),
                [0, 1, 3, 254, 255],
            )
            .unwrap(),
            _ => {}
        }
        let current_original = fs::read(original.join("Contents/MacOS/peerbrush")).unwrap();
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
            let selected = result.as_ref().unwrap();
            assert_eq!(selected.owner, copy);
            assert_eq!(selected.host, original);
            assert_eq!(selected.application, original);
            assert_eq!(
                selected.framework(),
                copy.join("Contents/Frameworks/Sparkle.framework")
            );
            selected
                .require_running_copy(&copy.join("Contents/MacOS/peerbrush"), &copy)
                .unwrap();
            assert!(selected
                .require_running_copy(&parent_exe, &original)
                .is_err());
        }
        if matches!(damage, "foreign-parent" | "wrong-digest" | "armed") {
            assert!(roles.is_empty());
        }
        assert_eq!(
            fs::read(original.join("Contents/MacOS/peerbrush")).unwrap(),
            current_original
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

fn current_profile_workspace(
    f: &Fixture,
) -> (
    crate::workspace::Registry,
    Vec<crate::server::Shared>,
    Vec<PathBuf>,
) {
    use crate::engine::{Document, Engine, Scope};
    use std::sync::{Arc, Mutex};
    let mut engine = Engine::new();
    engine.doc = Document::new_depth(4, 3, 8).unwrap();
    let root = Arc::new(Mutex::new(engine));
    let workspace = crate::workspace::attach(&root);
    let second = crate::workspace::new_project(&root, 4, 3, 16, None).unwrap();
    let projects = vec![root, second];
    let paths = vec![f.0.join("keep8.psd"), f.0.join("keep16.psd")];
    for (project, path) in projects.iter().zip(&paths) {
        {
            let mut e = project.lock().unwrap();
            e.doc.layers[0].pixels.set16(0, 0, [1001, 3003, 5007, 0]);
            let layer = e.doc.layers[0].id.clone();
            e.edit("human", &[serde_json::json!({"op":"fill","layer":layer,"rect":[1,1,3,3],"color":[41,83,127,173]})], None, None, "Keep undo").unwrap();
            e.edit(
                "human",
                &[serde_json::json!({"op":"layer.update","layer":layer,"name":"Keep redo"})],
                None,
                None,
                "Keep redo",
            )
            .unwrap();
            e.undo("human").unwrap();
            e.reserve("human", "Keep ownership", vec![Scope::layer(&layer)])
                .unwrap();
        }
        crate::server::save(project, path).unwrap();
    }
    (workspace, projects, paths)
}
fn current_workspace_bytes(
    projects: &[crate::server::Shared],
    paths: &[PathBuf],
) -> Vec<serde_json::Value> {
    projects.iter().zip(paths).map(|(project,path)| {
        let e = project.lock().unwrap();
        let history = |entries: &[crate::engine::History]| entries.iter().map(|h|serde_json::json!({
            "before":h.before,"after":h.after,"actor":h.actor,"label":h.label,"task":h.task,
            "scopes":h.scopes,"gesture":h.gesture,"reverted":h.reverted
        })).collect::<Vec<_>>();
        assert!(!e.undo.is_empty() && !e.redo.is_empty() && !e.leases.is_empty());
        serde_json::json!({"project":e.project_id,"document":e.doc,"undo":history(&e.undo),"redo":history(&e.redo),
            "leases":e.leases,"saved":e.saved_revision,"path":e.path,"closed":e.closed,
            "source":fs::read(path).unwrap(),"owner_path":e.file_ownership.as_ref().map(|owner|&owner.path),
            "owner_project":crate::file_ownership::local_project(path)})
    }).collect()
}

#[test]
fn actual_current_capture_preserves_later_flushed_settings_and_joint_native_sources_on_success_error_and_refusal(
) {
    for outcome in [
        "success",
        "dirty",
        "external",
        "operation",
        "unsaved",
        "copy-error",
        "unknown-native",
    ] {
        let f = Fixture::new();
        let profile = f.profile();
        let lease = profile.update_profile();
        let root = profile.locations.profile();
        let attempt = f.attempt(&lease);
        let initial = fs::read(attempt.join("checkpoint.json")).unwrap();
        fs::rename(
            attempt.join("checkpoint.json"),
            attempt.join("capture.json"),
        )
        .unwrap();
        let old_profile = fs::read(attempt.join("profile/tool-settings.json")).unwrap();
        let old_app = fs::read(attempt.join("PeerBrush.app/Contents/MacOS/peerbrush")).unwrap();
        let (workspace, projects, paths) = current_profile_workspace(&f);
        let reviewed = crate::workspace::lifecycle::update_readiness(&workspace).unwrap();
        let tools = workspace.lock().unwrap().tool_settings.clone();
        *tools.lock().unwrap() = crate::tool_settings::Store::load(root.join("tool-settings.json"));
        {
            let mut store = tools.lock().unwrap();
            let before = store.get("brush").unwrap().profile;
            let mut after = before.clone();
            after.settings.radius = 49.;
            store.human_delta("brush", &before, &after).unwrap();
            if outcome != "dirty" {
                store.flush().unwrap();
            }
        }
        let mut updates = crate::updates::Store::load(&root);
        let mut preferences = updates.preferences.clone();
        preferences.channel = crate::updates::Channel::Preview;
        updates.save(preferences).unwrap();
        if outcome == "external" {
            let file = root.join("tool-settings.json");
            let mut value: serde_json::Value =
                serde_json::from_slice(&fs::read(&file).unwrap()).unwrap();
            value["tools"]["brush"]["settings"]["radius"] = serde_json::json!(51.);
            fs::write(file, serde_json::to_vec_pretty(&value).unwrap()).unwrap();
        }
        if outcome == "unsaved" {
            projects[1].lock().unwrap().doc.revision += 1;
        }
        if outcome == "copy-error" {
            fs::create_dir(attempt.join("current-profile")).unwrap();
        }
        if outcome == "unknown-native" {
            write_new(
                &attempt.join("native-start-intent.json"),
                b"unknown native outcome",
            )
            .unwrap();
        }
        let operation = (outcome == "operation")
            .then(|| crate::server::FileOperation::begin_workspace(&workspace).unwrap());
        let expected = current_workspace_bytes(&projects, &paths);
        let owners: Vec<_> = projects
            .iter()
            .map(|p| p.lock().unwrap().file_ownership.clone().unwrap())
            .collect();
        let versions: Vec<_> = projects
            .iter()
            .map(|p| p.lock().unwrap().file_version)
            .collect();
        let current_tools = fs::read(root.join("tool-settings.json")).unwrap();
        let current_updates = fs::read(root.join("updates.json")).unwrap();
        let mut capture = None;
        let result = crate::workspace::lifecycle::publish_update_checkpoint(
            &workspace,
            &reviewed,
            |store| {
                assert!(tools.try_lock().is_err());
                capture_current_profile(&lease, store, &attempt, &initial, &mut capture)
            },
        );
        assert_eq!(
            result.is_ok(),
            outcome == "success",
            "{outcome}: {}",
            result.as_ref().err().map(String::as_str).unwrap_or("ok")
        );
        if let Some(capture) = capture {
            assert_ne!(current_tools, old_profile);
            assert_eq!(
                fs::read(attempt.join("current-profile/tool-settings.json")).unwrap(),
                current_tools
            );
            assert_eq!(
                fs::read(attempt.join("current-profile/updates.json")).unwrap(),
                current_updates
            );
            assert!(!attempt.join("current-profile/connection.json").exists());
            assert!(!attempt.join("current-profile/brushes.json").exists());
            assert_eq!(
                read_current_profile(&attempt, &initial).unwrap(),
                capture.record
            );
            capture.retire(&attempt).unwrap();
            capture.retire(&attempt).unwrap();
            // Model the real normal on_exit order: retire first, then retain its
            // later human delta and flush. Never restore from this older copy.
            let mut store = tools.lock().unwrap();
            let before = store.get("brush").unwrap().profile;
            let mut after = before.clone();
            after.settings.opacity = 0.63;
            store.human_delta("brush", &before, &after).unwrap();
            store.flush().unwrap();
            assert_eq!(store.get("brush").unwrap().profile.settings.opacity, 0.63);
            assert!(read_current_profile(&attempt, &initial).is_err());
            assert_eq!(
                fs::read(attempt.join("current-profile/tool-settings.json")).unwrap(),
                current_tools
            );
        } else {
            assert!(!attempt.join("current-profile.json").exists());
            assert!(!attempt.join("checkpoint.json").exists());
            assert_eq!(
                fs::read(root.join("tool-settings.json")).unwrap(),
                current_tools
            );
        }
        assert_eq!(fs::read(attempt.join("capture.json")).unwrap(), initial);
        assert_eq!(
            fs::read(attempt.join("profile/tool-settings.json")).unwrap(),
            old_profile
        );
        assert_eq!(
            fs::read(attempt.join("PeerBrush.app/Contents/MacOS/peerbrush")).unwrap(),
            old_app
        );
        assert_eq!(
            fs::read(root.join("updates.json")).unwrap(),
            current_updates
        );
        assert_eq!(current_workspace_bytes(&projects, &paths), expected);
        for (index, (project, owner)) in projects.iter().zip(&owners).enumerate() {
            let e = project.lock().unwrap();
            assert!(std::sync::Arc::ptr_eq(
                e.file_ownership.as_ref().unwrap(),
                owner
            ));
            assert_eq!(e.file_version, versions[index]);
            assert!(crate::file_ownership::Ownership::open(&paths[index], "foreign").is_err());
        }
        assert_eq!(crate::workspace::entries_in(&workspace).len(), 2);
        assert!(!workspace.lock().unwrap().exiting);
        assert!(require_captured_handoff().is_err());
        if outcome == "unknown-native" {
            assert_eq!(
                read_marker(&attempt.join("native-start-intent.json")).unwrap(),
                b"unknown native outcome"
            );
        } else {
            assert!(!attempt.join("native-start-intent.json").exists());
        }
        assert!(!attempt.join("native-not-started.json").exists());
        drop(operation);
    }
}

#[test]
fn current_capture_consumer_refuses_changed_copied_bytes_wrong_parent_and_retired_claims() {
    for damage in ["copy", "parent", "record", "retired-link"] {
        let f = Fixture::new();
        let profile = f.profile();
        let lease = profile.update_profile();
        let root = profile.locations.profile();
        let attempt = f.attempt(&lease);
        let initial = fs::read(attempt.join("checkpoint.json")).unwrap();
        fs::rename(
            attempt.join("checkpoint.json"),
            attempt.join("capture.json"),
        )
        .unwrap();
        let (workspace, projects, paths) = current_profile_workspace(&f);
        let tools = workspace.lock().unwrap().tool_settings.clone();
        *tools.lock().unwrap() = crate::tool_settings::Store::load(root.join("tool-settings.json"));
        let expected = current_workspace_bytes(&projects, &paths);
        let source = fs::read(root.join("tool-settings.json")).unwrap();
        let reviewed = crate::workspace::lifecycle::update_readiness(&workspace).unwrap();
        let mut captured = None;
        crate::workspace::lifecycle::publish_update_checkpoint(&workspace, &reviewed, |store| {
            capture_current_profile(&lease, store, &attempt, &initial, &mut captured)
        })
        .unwrap();
        assert!(read_current_profile(&attempt, &initial).is_ok());
        match damage {
            "copy" => fs::write(
                attempt.join("current-profile/tool-settings.json"),
                b"changed backup",
            )
            .unwrap(),
            "parent" => {
                fs::write(attempt.join("capture.json"), b"changed initial binding").unwrap()
            }
            "record" => fs::write(attempt.join("current-profile.json"), b"damaged record").unwrap(),
            "retired-link" => std::os::unix::fs::symlink(
                root.join("tool-settings.json"),
                attempt.join("current-profile-retired.json"),
            )
            .unwrap(),
            _ => unreachable!(),
        }
        assert!(read_current_profile(&attempt, &initial).is_err());
        if matches!(damage, "record" | "retired-link") {
            assert!(captured.unwrap().retire(&attempt).is_err());
        }
        assert_eq!(fs::read(root.join("tool-settings.json")).unwrap(), source);
        assert_eq!(current_workspace_bytes(&projects, &paths), expected);
        assert!(!workspace.lock().unwrap().exiting);
        assert!(require_captured_handoff().is_err());
        assert!(!attempt.join("native-start-intent.json").exists());
    }
}

#[test]
fn retirement_retry_repeats_file_and_parent_sync_before_releasing_matching_observation() {
    let f = Fixture::new();
    let profile = f.profile();
    let lease = profile.update_profile();
    let root = profile.locations.profile();
    let attempt = f.attempt(&lease);
    let initial = fs::read(attempt.join("checkpoint.json")).unwrap();
    fs::rename(
        attempt.join("checkpoint.json"),
        attempt.join("capture.json"),
    )
    .unwrap();
    let (workspace, projects, paths) = current_profile_workspace(&f);
    let expected = current_workspace_bytes(&projects, &paths);
    let owners: Vec<_> = projects
        .iter()
        .map(|p| p.lock().unwrap().file_ownership.clone().unwrap())
        .collect();
    let versions: Vec<_> = projects
        .iter()
        .map(|p| p.lock().unwrap().file_version)
        .collect();
    let tools = workspace.lock().unwrap().tool_settings.clone();
    *tools.lock().unwrap() = crate::tool_settings::Store::load(root.join("tool-settings.json"));
    let reviewed = crate::workspace::lifecycle::update_readiness(&workspace).unwrap();
    let mut capture = None;
    crate::workspace::lifecycle::publish_update_checkpoint(&workspace, &reviewed, |store| {
        capture_current_profile(&lease, store, &attempt, &initial, &mut capture)
    })
    .unwrap();
    let source = fs::read(root.join("tool-settings.json")).unwrap();
    let copied = fs::read(attempt.join("current-profile/tool-settings.json")).unwrap();
    let calls = std::cell::Cell::new(0);
    for expected_calls in [1, 2] {
        let result = capture
            .as_ref()
            .unwrap()
            .retire_with_sync(&attempt, |path| {
                assert_eq!(path, attempt.join("current-profile-retired.json"));
                calls.set(calls.get() + 1);
                Err("Injected retirement parent sync failure".into())
            });
        assert_eq!(
            result.unwrap_err(),
            "Injected retirement parent sync failure"
        );
        assert_eq!(calls.get(), expected_calls);
        assert!(
            capture.is_some(),
            "A failed sync cannot release the concrete observation"
        );
        assert!(
            read_marker(&attempt.join("current-profile-retired.json")).is_ok(),
            "Marker bytes exist even though directory publication failed"
        );
        assert_eq!(fs::read(root.join("tool-settings.json")).unwrap(), source);
        assert_eq!(current_workspace_bytes(&projects, &paths), expected);
    }
    capture
        .as_ref()
        .unwrap()
        .retire_with_sync(&attempt, |path| {
            calls.set(calls.get() + 1);
            crate::server::sync_parent(path)
        })
        .unwrap();
    assert_eq!(calls.get(), 3);
    capture.as_ref().unwrap().retire(&attempt).unwrap();
    assert!(read_current_profile(&attempt, &initial).is_err());
    assert_eq!(
        fs::read(attempt.join("current-profile/tool-settings.json")).unwrap(),
        copied
    );
    fs::write(
        attempt.join("current-profile-retired.json"),
        b"damaged retirement",
    )
    .unwrap();
    assert!(capture.as_ref().unwrap().retire(&attempt).is_err());
    assert_eq!(
        fs::read(attempt.join("current-profile-retired.json")).unwrap(),
        b"damaged retirement"
    );
    assert_eq!(fs::read(root.join("tool-settings.json")).unwrap(), source);
    assert_eq!(current_workspace_bytes(&projects, &paths), expected);
    for (index, (project, owner)) in projects.iter().zip(&owners).enumerate() {
        let e = project.lock().unwrap();
        assert!(std::sync::Arc::ptr_eq(
            e.file_ownership.as_ref().unwrap(),
            owner
        ));
        assert_eq!(e.file_version, versions[index]);
        assert!(crate::file_ownership::Ownership::open(&paths[index], "foreign").is_err());
    }
    assert!(!workspace.lock().unwrap().exiting);
    assert!(!attempt.join("native-start-intent.json").exists());
    assert!(require_captured_handoff().is_err());
}

#[test]
fn final_authentication_failure_after_durable_record_keeps_concrete_capture_retireable_and_sources_current(
) {
    for changed in ["tool-file", "absent-member"] {
        let f = Fixture::new();
        let profile = f.profile();
        let lease = profile.update_profile();
        let root = profile.locations.profile();
        let attempt = f.attempt(&lease);
        let initial = fs::read(attempt.join("checkpoint.json")).unwrap();
        fs::rename(
            attempt.join("checkpoint.json"),
            attempt.join("capture.json"),
        )
        .unwrap();
        let initial_profile = fs::read(attempt.join("profile/tool-settings.json")).unwrap();
        let (workspace, projects, paths) = current_profile_workspace(&f);
        let expected = current_workspace_bytes(&projects, &paths);
        let owners: Vec<_> = projects
            .iter()
            .map(|p| p.lock().unwrap().file_ownership.clone().unwrap())
            .collect();
        let versions: Vec<_> = projects
            .iter()
            .map(|p| p.lock().unwrap().file_version)
            .collect();
        let tools = workspace.lock().unwrap().tool_settings.clone();
        *tools.lock().unwrap() = crate::tool_settings::Store::load(root.join("tool-settings.json"));
        let reviewed = crate::workspace::lifecycle::update_readiness(&workspace).unwrap();
        let observed = std::rc::Rc::new(std::cell::Cell::new(false));
        let fired = observed.clone();
        let live = root.clone();
        let published = attempt.clone();
        let binding = initial.clone();
        let _watch = WatchCurrentRecord::new(move || {
            // The actual records/copy are already written, synced and strictly
            // readable. Now force the final real Store or source-stamp check.
            assert!(read_current_profile(&published, &binding).is_ok());
            fired.set(true);
            if changed == "tool-file" {
                let path = live.join("tool-settings.json");
                let mut value: serde_json::Value =
                    serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
                value["tools"]["brush"] = serde_json::to_value(crate::tool_settings::Profile {
                    settings: crate::brush::Settings {
                        radius: 57.,
                        ..Default::default()
                    },
                    ..Default::default()
                })
                .unwrap();
                fs::write(path, serde_json::to_vec_pretty(&value).unwrap()).unwrap();
            } else {
                assert!(!live.join("updates.json").exists());
                fs::write(
                    live.join("updates.json"),
                    serde_json::to_vec(&crate::updates::Preferences::default()).unwrap(),
                )
                .unwrap();
            }
            Ok(())
        });
        let mut capture = None;
        let result = crate::workspace::lifecycle::publish_update_checkpoint(
            &workspace,
            &reviewed,
            |store| capture_current_profile(&lease, store, &attempt, &initial, &mut capture),
        );
        assert!(observed.get());
        assert!(result.is_err(), "{changed}");
        assert!(
            capture.is_some(),
            "The retained Prepared slot must survive any post-publication error"
        );
        assert_eq!(
            read_current_profile(&attempt, &initial).unwrap(),
            capture.as_ref().unwrap().record
        );
        let current = fs::read(root.join("tool-settings.json")).unwrap();
        let current_update = fs::read(root.join("updates.json")).ok();
        assert!(lease.validate().is_ok());
        capture.as_ref().unwrap().retire(&attempt).unwrap();
        assert!(read_current_profile(&attempt, &initial).is_err());
        assert_eq!(fs::read(root.join("tool-settings.json")).unwrap(), current);
        assert_eq!(fs::read(root.join("updates.json")).ok(), current_update);
        assert_eq!(
            fs::read(attempt.join("profile/tool-settings.json")).unwrap(),
            initial_profile
        );
        assert_eq!(fs::read(attempt.join("capture.json")).unwrap(), initial);
        assert_eq!(
            fs::read(attempt.join("current-profile/tool-settings.json")).unwrap(),
            initial_profile
        );
        assert_eq!(current_workspace_bytes(&projects, &paths), expected);
        for (index, (project, owner)) in projects.iter().zip(&owners).enumerate() {
            let e = project.lock().unwrap();
            assert!(std::sync::Arc::ptr_eq(
                e.file_ownership.as_ref().unwrap(),
                owner
            ));
            assert_eq!(e.file_version, versions[index]);
            assert!(crate::file_ownership::Ownership::open(&paths[index], "foreign").is_err());
        }
        assert_eq!(crate::workspace::entries_in(&workspace).len(), 2);
        assert!(!workspace.lock().unwrap().exiting);
        assert!(!attempt.join("native-start-intent.json").exists());
        assert!(!attempt.join("native-not-started.json").exists());
        assert!(require_captured_handoff().is_err());
    }
}

#[test]
fn copied_owner_roles_refuse_target_running_bundle_role_swaps_and_nonabsolute_paths() {
    let f = Fixture::new();
    let owner = f.0.join("capture/PeerBrush.app");
    let target = f.0.join("original.app");
    let selected = || OwnerBundles {
        owner: owner.clone(),
        host: target.clone(),
        application: target.clone(),
    };
    let executable = owner.join("Contents/MacOS/peerbrush");
    selected()
        .require_running_copy(&executable, &owner)
        .unwrap();
    for swapped in [
        "owner",
        "host",
        "application",
        "relative",
        "main",
        "executable",
    ] {
        let mut bundles = selected();
        let mut running = executable.clone();
        let mut main = owner.clone();
        match swapped {
            "owner" => bundles.owner = target.clone(),
            "host" => bundles.host = owner.clone(),
            "application" => bundles.application = owner.clone(),
            "relative" => bundles.host = "original.app".into(),
            "main" => main = target.clone(),
            "executable" => running = target.join("Contents/MacOS/peerbrush"),
            _ => unreachable!(),
        }
        assert!(
            bundles.require_running_copy(&running, &main).is_err(),
            "{swapped}"
        );
    }
    // Role checks alone are never captured-handoff/native start authority.
    assert!(require_captured_handoff().is_err());
    assert!(!owner.exists() && !target.exists());
}

#[test]
fn installed_host_root_ownership_does_not_relax_private_capture_ownership() {
    for (uid, installed, copied) in [(0, true, false), (501, true, true), (502, false, false)] {
        assert_eq!(TreeOwner::InstalledHost.permits(uid, 501), installed);
        assert_eq!(TreeOwner::PrivateCapture.permits(uid, 501), copied);
    }
}

#[test]
fn retained_armed_owner_rechecks_its_exact_records_without_relaxing_new_owner_admission() {
    for damage in [
        "none",
        "intent",
        "receipt",
        "unstarted",
        "other",
        "source",
        "copy",
        "profile",
        "during",
    ] {
        let f = Fixture::new();
        let profile = f.profile();
        let lease = profile.update_profile();
        let attempt = f.attempt(&lease);
        let initial = fs::read(attempt.join("checkpoint.json")).unwrap();
        fs::rename(
            attempt.join("checkpoint.json"),
            attempt.join("capture.json"),
        )
        .unwrap();
        let (workspace, projects, paths) = current_profile_workspace(&f);
        let tools = workspace.lock().unwrap().tool_settings.clone();
        *tools.lock().unwrap() = crate::tool_settings::Store::load(
            profile.locations.profile().join("tool-settings.json"),
        );
        let reviewed = crate::workspace::lifecycle::update_readiness(&workspace).unwrap();
        let mut capture = None;
        crate::workspace::lifecycle::publish_update_checkpoint(&workspace, &reviewed, |store| {
            capture_current_profile(&lease, store, &attempt, &initial, &mut capture)
        })
        .unwrap();
        let expected = current_workspace_bytes(&projects, &paths);
        // Filesystem receipt injection only: no native owner/check is faked into
        // authority. The actual factory and native recheck remain source-refused.
        record_intent(&attempt, &initial).unwrap();
        let mut armed = ArmedCheck {
            app: attempt.join("PeerBrush.app"),
            manifest: initial.clone(),
            intent: read_marker(&attempt.join("native-start-intent.json")).unwrap(),
            owner_pid: std::process::id(),
        };
        let configuration = ApprovedCheck {
            feed_url: crate::sparkle::PROPOSED_FEED_URL.into(),
            public_ed_key: "public fixture, no native authority".into(),
            publisher_team_id: "ABCD123456".into(),
            objects: BTreeMap::new(),
        };
        let stored: Manifest = serde_json::from_slice(&initial).unwrap();
        let parent = stored.target.join("Contents/MacOS/peerbrush");
        assert!(startup_guard(&profile.locations).is_err());
        assert!(inspect_owner_copy_with(
            &armed.app,
            &lease,
            &parent,
            &digest(&initial),
            &configuration,
            &AtomicBool::new(false),
            |_| panic!("Armed state reached new-owner native verification")
        )
        .is_err());
        assert_eq!(
            armed
                .recheck(
                    &lease,
                    &parent,
                    configuration.clone(),
                    Arc::new(AtomicBool::new(false))
                )
                .err()
                .unwrap(),
            require_captured_handoff().unwrap_err()
        );
        match damage {
            "intent" => {
                fs::write(attempt.join("native-start-intent.json"), b"changed receipt").unwrap()
            }
            "receipt" => armed.manifest.push(b' '),
            "unstarted" => record_unstarted(&attempt, &initial).unwrap(),
            "other" => {
                let other = checkpoints(&profile.locations).join(uuid::Uuid::new_v4().to_string());
                fs::DirBuilder::new().mode(0o700).create(&other).unwrap();
                record_intent(&other, &initial).unwrap();
            }
            "source" => fs::write(&parent, b"changed original").unwrap(),
            "copy" => {
                fs::write(armed.app.join("Contents/MacOS/peerbrush"), b"changed copy").unwrap()
            }
            "profile" => fs::write(
                attempt.join("current-profile/tool-settings.json"),
                b"changed profile",
            )
            .unwrap(),
            _ => {}
        }
        let calls = std::cell::Cell::new(0);
        let result = armed.recheck_with(
            &lease,
            &parent,
            &configuration,
            &AtomicBool::new(false),
            |app| {
                assert!(app == stored.target || app == armed.app);
                calls.set(calls.get() + 1);
                if damage == "during" && calls.get() == 1 {
                    fs::write(
                        attempt.join("native-start-intent.json"),
                        b"changed during verification",
                    )
                    .unwrap();
                }
                Ok(())
            },
        );
        assert_eq!(result.is_ok(), damage == "none", "{damage}");
        if let Ok(bundles) = result {
            assert_eq!(bundles.owner, armed.app);
            assert_eq!(bundles.host, stored.target);
            assert_eq!(bundles.application, stored.target);
            assert_eq!(calls.get(), 2);
        }
        assert_eq!(current_workspace_bytes(&projects, &paths), expected);
        assert!(!workspace.lock().unwrap().exiting);
        assert!(attempt.join("native-start-intent.json").exists());
        assert!(require_captured_handoff().is_err());
        // Only the exact never-started outcome permits a later profile startup;
        // all active, unknown or damaged attempts remain held.
        assert_eq!(
            startup_guard(&profile.locations).is_ok(),
            damage == "unstarted",
            "{damage}"
        );
    }
}

#[test]
fn actual_final_profile_observation_reuses_writer_authority_and_preserves_current_joint_native_work(
) {
    use super::super::{
        accept_final_control, accept_final_observation, coordinator, driver, request_final_control,
        request_final_observation,
    };
    use crate::source::{Content, Source};
    use serde_json::json;
    for outcome in [
        "success",
        "control-newer-source",
        "blocked-before",
        "dirty-before",
        "unsaved-before",
        "operation-before",
        "copy-error",
        "post-record-error",
        "send-error",
        "newer-source",
        "dirty-after",
        "present-after",
        "absent-after",
        "removed-after",
        "receipt-damage",
        "copy-damage",
        "intent-damage",
        "wrong-ready",
        "wrong-sources",
        "wrong-record",
        "cancel-after",
        "blocked-after",
    ] {
        let f = Fixture::new();
        let profile = f.profile();
        let lease = profile.update_profile();
        let root = profile.locations.profile();
        let attempt = f.attempt(&lease);
        let initial = fs::read(attempt.join("checkpoint.json")).unwrap();
        fs::rename(
            attempt.join("checkpoint.json"),
            attempt.join("capture.json"),
        )
        .unwrap();
        let original_profile = fs::read(attempt.join("profile/tool-settings.json")).unwrap();
        let original_app =
            fs::read(attempt.join("PeerBrush.app/Contents/MacOS/peerbrush")).unwrap();
        let (workspace, projects, paths) = current_profile_workspace(&f);
        // Keep a real editable source as well as hidden native pixels and both
        // nonempty histories; saves use the real source/revision consent route.
        for (project, path) in projects.iter().zip(&paths) {
            let (project_id, document, revision) = {
                let mut e = project.lock().unwrap();
                let raster = e.doc.layers[0].id.clone();
                let source = Source {
                    width: 4,
                    height: 3,
                    matrix: [1., 0., 0., 1., 0., 0.],
                    content: Content::Shape {
                        shape: "rectangle".into(),
                        bounds: [1., 1., 2., 2.],
                        points: vec![],
                        closed: true,
                        fill: [12501, 23003, 34507, 45611],
                        stroke: [0; 4],
                        stroke_width: 0.,
                    },
                };
                e.edit(
                    "human",
                    &[json!({"op":"source.add","source":source,"x":0,"y":0})],
                    None,
                    None,
                    "Keep editable source",
                )
                .unwrap();
                for name in ["Keep current name", "Undo temporary name"] {
                    e.edit(
                        "human",
                        &[json!({"op":"layer.update","layer":raster,"name":name})],
                        None,
                        None,
                        "Human name",
                    )
                    .unwrap();
                }
                e.undo("human").unwrap();
                assert!(e.doc.layers.iter().any(|layer| layer.source.is_some()));
                let hidden = e
                    .doc
                    .layers
                    .iter()
                    .find(|layer| layer.id == raster)
                    .unwrap()
                    .pixels
                    .get16(0, 0);
                assert_eq!(hidden[3], 0);
                assert!(hidden[..3].iter().any(|v| *v != 0));
                if e.doc.bit_depth == 16 {
                    assert!(hidden[..3].iter().any(|v| v % 257 != 0));
                }
                (e.project_id.clone(), e.doc.id.clone(), e.doc.revision)
            };
            let report = crate::server::dispatch(
                project,
                "document",
                &json!({"action":"save_report",
                "project_id":project_id,"document_id":document,"expected_revision":revision}),
            )
            .unwrap();
            assert_eq!(report["document_id"], document);
            assert_eq!(report["revision"], revision);
            assert_eq!(report["save_allowed"], true);
            assert!(report["losses"]
                .as_array()
                .unwrap()
                .iter()
                .any(|loss| loss["code"] == "editable_source_rasterized"));
            crate::server::save_accepted_source(project, path, &document, revision, false).unwrap();
            let e = project.lock().unwrap();
            let reopened = crate::psd::decode(&fs::read(path).unwrap()).unwrap();
            assert_eq!(reopened.bit_depth, e.doc.bit_depth);
            assert_eq!(reopened.export_png().unwrap(), e.doc.export_png().unwrap());
        }
        let tools = workspace.lock().unwrap().tool_settings.clone();
        *tools.lock().unwrap() = crate::tool_settings::Store::load(root.join("tool-settings.json"));
        let reviewed = crate::workspace::lifecycle::update_readiness(&workspace).unwrap();
        let mut current = None;
        crate::workspace::lifecycle::publish_update_checkpoint(&workspace, &reviewed, |store| {
            capture_current_profile(&lease, store, &attempt, &initial, &mut current)
        })
        .unwrap();
        let pre_cycle = fs::read(attempt.join("current-profile/tool-settings.json")).unwrap();
        // Only the process/native boundary is injected. The production factory
        // remains unconditionally refused; these files cannot authorize startup.
        record_intent(&attempt, &initial).unwrap();
        let armed = ArmedCheck {
            app: attempt.join("PeerBrush.app"),
            manifest: initial.clone(),
            intent: read_marker(&attempt.join("native-start-intent.json")).unwrap(),
            owner_pid: std::process::id(),
        };
        let checks = driver::State::default();
        checks.session.borrow_mut().begin().unwrap();
        checks.session.borrow_mut().checking();
        checks.native_starting().unwrap();
        let status = coordinator::NativeStatus {
            phase: crate::sparkle::check::Phase::Quarantined,
            version: Some("0.30.1".into()),
            busy: false,
            cancelled: false,
            quarantine: true,
            native_cycle: true,
            held_generation: Some(7),
            found_generation: None,
            download_generation: None,
            expected_bytes: None,
            received_bytes: 0,
            extraction: None,
            download_complete: false,
            final_profile: None,
            outcome: coordinator::NativeOutcome::Unknown,
        };
        checks.observe_owner(status.clone());
        {
            let mut store = tools.lock().unwrap();
            let before = store.get("brush").unwrap().profile;
            let mut after = before.clone();
            after.settings.radius = 49.;
            store.human_delta("brush", &before, &after).unwrap();
            if outcome != "dirty-before" {
                store.flush().unwrap();
            }
        }
        let mut updates = crate::updates::Store::load(&root);
        let mut preferences = updates.preferences.clone();
        preferences.channel = crate::updates::Channel::Preview;
        updates.save(preferences).unwrap();
        let current_tools = fs::read(root.join("tool-settings.json")).unwrap();
        let current_updates = fs::read(root.join("updates.json")).unwrap();
        assert_eq!(current_tools == pre_cycle, outcome == "dirty-before");
        assert_eq!(
            tools.lock().unwrap().state()["pending_save"],
            outcome == "dirty-before"
        );
        if outcome == "unsaved-before" {
            projects[1].lock().unwrap().doc.revision += 1;
        }
        if outcome == "copy-error" {
            fs::create_dir(attempt.join("final-profile")).unwrap();
        }
        let operation = (outcome == "operation-before")
            .then(|| crate::server::FileOperation::begin_workspace(&workspace).unwrap());
        let record_path = attempt.join("final-profile.json");
        let _watch = (outcome == "post-record-error").then(|| {
            WatchProfile::new(move |_| {
                if record_path.exists() {
                    Err("Injected final post-record authentication failure".into())
                } else {
                    Ok(())
                }
            })
        });
        let workspace_before = current_workspace_bytes(&projects, &paths);
        let owners_before: Vec<_> = projects
            .iter()
            .map(|p| p.lock().unwrap().file_ownership.clone().unwrap())
            .collect();
        let versions_before: Vec<_> = projects
            .iter()
            .map(|p| p.lock().unwrap().file_version)
            .collect();
        let mut final_capture = None;
        let mut retained = None;
        let sends = std::cell::Cell::new(0);
        let result = request_final_observation(
            &checks,
            Some(&status),
            &mut retained,
            outcome == "blocked-before",
            |generation| {
                let reviewed = crate::workspace::lifecycle::update_readiness(&workspace)?;
                crate::workspace::lifecycle::publish_update_checkpoint(
                    &workspace,
                    &reviewed,
                    |store| {
                        assert!(tools.try_lock().is_err());
                        capture_final_profile(
                            &lease,
                            store,
                            &attempt,
                            &initial,
                            std::process::id(),
                            generation,
                            &reviewed,
                            &mut final_capture,
                        )
                    },
                )?;
                Ok(final_capture.as_ref().unwrap().binding.clone())
            },
            |binding| {
                sends.set(sends.get() + 1);
                // Exercise the actual strict copied-owner consumer, not a fabricated
                // successful acknowledgement of settings bytes.
                armed.observe_final_profile(&lease, &binding)?;
                if outcome == "send-error" {
                    return Err("Injected lost observation reply".into());
                }
                Ok(())
            },
        );
        drop(_watch);
        drop(operation);
        assert_eq!(
            current_workspace_bytes(&projects, &paths),
            workspace_before,
            "{outcome}"
        );
        for (index, project) in projects.iter().enumerate() {
            let e = project.lock().unwrap();
            assert!(!e.closed);
            assert!(Arc::ptr_eq(
                e.file_ownership.as_ref().unwrap(),
                &owners_before[index]
            ));
            assert_eq!(e.file_version, versions_before[index]);
        }
        let before_refusal = matches!(
            outcome,
            "blocked-before"
                | "dirty-before"
                | "unsaved-before"
                | "operation-before"
                | "copy-error"
                | "post-record-error"
        );
        assert_eq!(
            result.is_ok(),
            !before_refusal && outcome != "send-error",
            "{outcome}: {result:?}"
        );
        if outcome == "post-record-error" {
            assert!(final_capture.is_some() && retained.is_none());
            read_final_profile(
                &attempt,
                &initial,
                std::process::id(),
                &final_capture.as_ref().unwrap().binding,
            )
            .unwrap();
        }
        if let Some(capture) = &final_capture {
            assert_eq!(
                fs::read(attempt.join("final-profile/tool-settings.json")).unwrap(),
                current_tools
            );
            assert_eq!(
                fs::read(attempt.join("final-profile/updates.json")).unwrap(),
                current_updates
            );
            assert!(capture
                .current
                .entries
                .get(Path::new("brushes.json"))
                .is_none());
            assert!(request_final_observation(
                &checks,
                Some(&status),
                &mut retained,
                false,
                |generation| {
                    let reviewed = crate::workspace::lifecycle::update_readiness(&workspace)?;
                    crate::workspace::lifecycle::publish_update_checkpoint(
                        &workspace,
                        &reviewed,
                        |store| {
                            capture_final_profile(
                                &lease,
                                store,
                                &attempt,
                                &initial,
                                std::process::id(),
                                generation,
                                &reviewed,
                                &mut final_capture,
                            )
                        },
                    )?;
                    unreachable!("A retained concrete capture cannot be republished")
                },
                |_| panic!("Repeated final observation reached transport")
            )
            .is_err());
        }
        let published = fs::read(attempt.join("final-profile.json")).ok();
        let source_before = current_workspace_bytes(&projects, &paths);
        if result.is_ok() {
            let mut processed = false;
            assert!(accept_final_observation(
                &checks,
                &status,
                retained.as_ref(),
                &mut processed,
                false,
                |_| panic!("Absent reply reached consumer")
            )
            .is_none());
            let mut reply = status.clone();
            reply.final_profile = retained.clone();
            match outcome {
                "newer-source" => {
                    let mut e = projects[1].lock().unwrap();
                    let raster = e
                        .doc
                        .layers
                        .iter()
                        .find(|l| l.source.is_none())
                        .unwrap()
                        .id
                        .clone();
                    for name in ["Newer human work", "Undo newer temporary"] {
                        e.edit(
                            "human",
                            &[json!({"op":"layer.update","layer":raster,"name":name})],
                            None,
                            None,
                            "Newer work",
                        )
                        .unwrap();
                    }
                    e.undo("human").unwrap();
                }
                "dirty-after" => {
                    let mut store = tools.lock().unwrap();
                    let before = store.get("brush").unwrap().profile;
                    let mut after = before.clone();
                    after.settings.opacity = 0.63;
                    store.human_delta("brush", &before, &after).unwrap();
                }
                "present-after" => {
                    let mut preferences = updates.preferences.clone();
                    preferences.channel = crate::updates::Channel::Stable;
                    updates.save(preferences).unwrap();
                }
                "absent-after" => write_new(&root.join("brushes.json"), b"[]").unwrap(),
                "removed-after" => fs::remove_file(root.join("updates.json")).unwrap(),
                "receipt-damage" => {
                    fs::write(attempt.join("final-profile.json"), b"damaged record").unwrap()
                }
                "copy-damage" => fs::write(
                    attempt.join("final-profile/tool-settings.json"),
                    b"damaged copy",
                )
                .unwrap(),
                "intent-damage" => {
                    fs::write(attempt.join("native-start-intent.json"), b"changed intent").unwrap()
                }
                "wrong-ready" => {
                    reply.held_generation = Some(8);
                    reply.final_profile.as_mut().unwrap().ready_generation = 8;
                }
                "wrong-sources" => {
                    reply.final_profile.as_mut().unwrap().sources_sha256 = "c".repeat(64)
                }
                "wrong-record" => {
                    reply.final_profile.as_mut().unwrap().record_sha256 = "d".repeat(64)
                }
                "cancel-after" => checks.cancel(),
                _ => {}
            }
            let expected = current_workspace_bytes(&projects, &paths);
            let owners: Vec<_> = projects
                .iter()
                .map(|p| p.lock().unwrap().file_ownership.clone().unwrap())
                .collect();
            let versions: Vec<_> = projects
                .iter()
                .map(|p| p.lock().unwrap().file_version)
                .collect();
            let current_disk: Vec<_> = crate::settings::update_files()
                .iter()
                .map(|name| (name.clone(), fs::read(root.join(name)).ok()))
                .collect();
            let accepted = accept_final_observation(
                &checks,
                &reply,
                retained.as_ref(),
                &mut processed,
                outcome == "blocked-after",
                |binding| {
                    final_capture.as_ref().unwrap().accept(
                        &workspace,
                        &lease,
                        &attempt,
                        &initial,
                        std::process::id(),
                        binding,
                    )
                },
            )
            .unwrap();
            assert_eq!(
                accepted.is_ok(),
                matches!(outcome, "success" | "control-newer-source"),
                "{outcome}: {accepted:?}"
            );
            assert!(processed);
            assert!(accept_final_observation(
                &checks,
                &reply,
                retained.as_ref(),
                &mut processed,
                false,
                |_| panic!("Repeated reply reached consumer")
            )
            .is_none());
            assert_eq!(
                current_workspace_bytes(&projects, &paths),
                expected,
                "{outcome}"
            );
            for (index, project) in projects.iter().enumerate() {
                let e = project.lock().unwrap();
                assert!(!e.closed);
                assert!(Arc::ptr_eq(
                    e.file_ownership.as_ref().unwrap(),
                    &owners[index]
                ));
                assert_eq!(e.file_version, versions[index]);
                assert!(crate::file_ownership::Ownership::open(&paths[index], "foreign").is_err());
            }
            for (name, bytes) in current_disk {
                assert_eq!(fs::read(root.join(name)).ok(), bytes);
            }
            if outcome == "success" {
                assert_eq!(expected, source_before);
                let capture = final_capture.as_ref().unwrap();
                let binding = retained.as_ref().unwrap();
                for control_case in [
                    "success",
                    "blocked",
                    "unaccepted",
                    "send-error",
                    "wrong-reply",
                    "late-cancel",
                    "newer-ready",
                ] {
                    let control_checks = driver::State::default();
                    control_checks.session.borrow_mut().begin().unwrap();
                    control_checks.session.borrow_mut().checking();
                    control_checks.native_starting().unwrap();
                    let mut requested = None;
                    let control_sends = std::cell::Cell::new(0);
                    let auth = std::cell::Cell::new(0);
                    let authenticate = |selected: &FinalProfileBinding| {
                        auth.set(auth.get() + 1);
                        crate::workspace::lifecycle::publish_update_checkpoint(
                            &workspace,
                            &capture.reviewed,
                            |store| {
                                capture.authenticate_locked(
                                    &lease,
                                    &attempt,
                                    &initial,
                                    std::process::id(),
                                    selected,
                                    store,
                                )
                            },
                        )
                    };
                    let result = request_final_control(
                        &control_checks,
                        Some(&reply),
                        (control_case != "unaccepted").then_some(binding),
                        &mut requested,
                        control_case == "blocked",
                        authenticate,
                        |selected| {
                            assert_eq!(&selected, binding);
                            control_sends.set(control_sends.get() + 1);
                            if control_case == "send-error" {
                                Err("Ambiguous transport fixture".into())
                            } else {
                                Ok(())
                            }
                        },
                    );
                    assert_eq!(
                        result.is_ok(),
                        !matches!(control_case, "blocked" | "unaccepted" | "send-error"),
                        "{control_case}"
                    );
                    assert_eq!(
                        requested.is_some(),
                        !matches!(control_case, "blocked" | "unaccepted")
                    );
                    assert_eq!(
                        auth.get(),
                        usize::from(!matches!(control_case, "blocked" | "unaccepted"))
                    );
                    assert_eq!(control_sends.get(), auth.get());
                    if result.is_ok() {
                        let mut observed = reply.clone();
                        let mut acknowledged = binding.clone();
                        if control_case == "wrong-reply" {
                            acknowledged.record_sha256 = "e".repeat(64);
                        }
                        if control_case == "late-cancel" {
                            control_checks.cancel();
                        }
                        if control_case == "newer-ready" {
                            observed.held_generation = Some(binding.ready_generation + 1);
                        }
                        let mut consumed = false;
                        let accepted = accept_final_control(
                            &control_checks,
                            Some(&observed),
                            requested.as_ref(),
                            &acknowledged,
                            &mut consumed,
                            false,
                            authenticate,
                        );
                        assert_eq!(
                            accepted.is_ok(),
                            control_case == "success",
                            "{control_case}"
                        );
                        assert!(consumed);
                        assert!(accept_final_control(
                            &control_checks,
                            Some(&reply),
                            requested.as_ref(),
                            binding,
                            &mut consumed,
                            false,
                            |_| panic!("Repeated control reached writer authority")
                        )
                        .is_err());
                    }
                    if requested.is_some() {
                        assert!(request_final_control(
                            &control_checks,
                            Some(&reply),
                            Some(binding),
                            &mut requested,
                            false,
                            |_| panic!("Repeated request reached writer authority"),
                            |_| panic!("Repeated control reached transport")
                        )
                        .is_err());
                    }
                    assert_eq!(
                        current_workspace_bytes(&projects, &paths),
                        expected,
                        "{control_case}"
                    );
                    assert!(!workspace.lock().unwrap().exiting);
                    assert!(!tools.lock().unwrap().update_exit_committed());
                    for (index, project) in projects.iter().enumerate() {
                        let e = project.lock().unwrap();
                        assert!(
                            !e.closed
                                && Arc::ptr_eq(e.file_ownership.as_ref().unwrap(), &owners[index])
                        );
                        assert_eq!(e.file_version, versions[index]);
                        assert!(crate::file_ownership::Ownership::open(
                            &paths[index],
                            "control-foreign"
                        )
                        .is_err());
                    }
                }
                // No-later-writer exclusion is deliberately still unfinished:
                // a later real preference write remains available and makes the
                // old observation fail if explicitly reauthenticated.
                let mut store = tools.lock().unwrap();
                let before = store.get("brush").unwrap().profile;
                let mut after = before.clone();
                after.settings.opacity = 0.57;
                store.human_delta("brush", &before, &after).unwrap();
                store.flush().unwrap();
                drop(store);
                assert!(final_capture
                    .as_ref()
                    .unwrap()
                    .accept(
                        &workspace,
                        &lease,
                        &attempt,
                        &initial,
                        std::process::id(),
                        retained.as_ref().unwrap()
                    )
                    .is_err());
                let mut control = None;
                assert!(request_final_control(
                    &checks,
                    Some(&reply),
                    retained.as_ref(),
                    &mut control,
                    false,
                    |binding| crate::workspace::lifecycle::publish_update_checkpoint(
                        &workspace,
                        &capture.reviewed,
                        |store| {
                            capture.authenticate_locked(
                                &lease,
                                &attempt,
                                &initial,
                                std::process::id(),
                                binding,
                                store,
                            )
                        }
                    ),
                    |_| panic!("Later human settings reached control transport")
                )
                .is_err());
                assert!(control.is_none());
                assert_eq!(current_workspace_bytes(&projects, &paths), expected);
            }
            if outcome == "control-newer-source" {
                let capture = final_capture.as_ref().unwrap();
                let binding = retained.as_ref().unwrap();
                let authenticate = |selected: &FinalProfileBinding| {
                    crate::workspace::lifecycle::publish_update_checkpoint(
                        &workspace,
                        &capture.reviewed,
                        |store| {
                            capture.authenticate_locked(
                                &lease,
                                &attempt,
                                &initial,
                                std::process::id(),
                                selected,
                                store,
                            )
                        },
                    )
                };
                let mut requested = None;
                request_final_control(
                    &checks,
                    Some(&reply),
                    Some(binding),
                    &mut requested,
                    false,
                    authenticate,
                    |selected| {
                        assert_eq!(&selected, binding);
                        Ok(())
                    },
                )
                .unwrap();
                {
                    let mut e = projects[1].lock().unwrap();
                    let raster = e
                        .doc
                        .layers
                        .iter()
                        .find(|layer| layer.source.is_none())
                        .unwrap()
                        .id
                        .clone();
                    for name in [
                        "Human work after final request",
                        "Undo temporary control name",
                    ] {
                        e.edit(
                            "human",
                            &[json!({"op":"layer.update","layer":raster,"name":name})],
                            None,
                            None,
                            "Newer human control work",
                        )
                        .unwrap();
                    }
                    e.undo("human").unwrap();
                    assert!(!e.undo.is_empty() && !e.redo.is_empty());
                }
                let newer = current_workspace_bytes(&projects, &paths);
                assert_ne!(newer, expected);
                let mut consumed = false;
                assert!(accept_final_control(
                    &checks,
                    Some(&reply),
                    requested.as_ref(),
                    binding,
                    &mut consumed,
                    false,
                    authenticate,
                )
                .is_err());
                assert!(consumed);
                assert_eq!(current_workspace_bytes(&projects, &paths), newer);
                assert!(!workspace.lock().unwrap().exiting);
                assert!(!tools.lock().unwrap().update_exit_committed());
                for (index, project) in projects.iter().enumerate() {
                    let e = project.lock().unwrap();
                    assert!(!e.closed);
                    assert!(Arc::ptr_eq(
                        e.file_ownership.as_ref().unwrap(),
                        &owners[index]
                    ));
                    assert_eq!(e.file_version, versions[index]);
                    assert!(crate::file_ownership::Ownership::open(
                        &paths[index],
                        "newer-control-foreign"
                    )
                    .is_err());
                }
            }
        } else {
            assert_eq!(current_workspace_bytes(&projects, &paths), source_before);
        }
        if outcome != "receipt-damage" {
            assert_eq!(fs::read(attempt.join("final-profile.json")).ok(), published);
        }
        assert_eq!(fs::read(attempt.join("capture.json")).unwrap(), initial);
        assert_eq!(
            fs::read(attempt.join("profile/tool-settings.json")).unwrap(),
            original_profile
        );
        assert_eq!(
            fs::read(attempt.join("current-profile/tool-settings.json")).unwrap(),
            pre_cycle
        );
        assert_eq!(
            fs::read(attempt.join("PeerBrush.app/Contents/MacOS/peerbrush")).unwrap(),
            original_app
        );
        assert!(checks.view().native_cycle && checks.view().quarantine);
        assert!(!workspace.lock().unwrap().exiting);
        assert_eq!(crate::workspace::entries_in(&workspace).len(), 2);
        assert!(!attempt.join("native-not-started.json").exists());
        assert!(startup_guard(&profile.locations).is_err());
        assert!(require_captured_handoff().is_err());
        assert_eq!(
            sends.get(),
            if result.is_ok() || outcome == "send-error" {
                1
            } else {
                0
            }
        );
    }
}

#[test]
fn planned_update_exit_seals_current_joint_sources_only_after_final_owner_authentication() {
    use super::super::{accept_final_control, coordinator, driver};
    for outcome in [
        "success",
        "unsaved",
        "dirty-tools",
        "copy-changed",
        "owner-gone",
        "operation",
        "newer-ready",
        "quarantined",
        "cancel",
        "blocked",
    ] {
        let f = Fixture::new();
        let profile = f.profile();
        let lease = profile.update_profile();
        let attempt = f.attempt(&lease);
        let initial = fs::read(attempt.join("checkpoint.json")).unwrap();
        fs::rename(
            attempt.join("checkpoint.json"),
            attempt.join("capture.json"),
        )
        .unwrap();
        let (workspace, projects, paths) = current_profile_workspace(&f);
        let tools = workspace.lock().unwrap().tool_settings.clone();
        *tools.lock().unwrap() = crate::tool_settings::Store::load(
            profile.locations.profile().join("tool-settings.json"),
        );
        for (project, path) in projects.iter().zip(&paths) {
            let (project_id, document, revision) = {
                let mut e = project.lock().unwrap();
                let raster = e.doc.layers[0].id.clone();
                let source = crate::source::Source {
                    width: 4,
                    height: 3,
                    matrix: [1., 0., 0., 1., 0., 0.],
                    content: crate::source::Content::Shape {
                        shape: "rectangle".into(),
                        bounds: [1., 1., 2., 2.],
                        points: vec![],
                        closed: true,
                        fill: [12501, 23003, 34507, 45611],
                        stroke: [0; 4],
                        stroke_width: 0.,
                    },
                };
                e.edit(
                    "human",
                    &[serde_json::json!({"op":"source.add","source":source,"x":0,"y":0})],
                    None,
                    None,
                    "Keep editable source",
                )
                .unwrap();
                e.edit("human", &[serde_json::json!({"op":"layer.update","layer":raster,"name":"Temporary exit name"})], None, None, "Keep redo").unwrap();
                e.undo("human").unwrap();
                (e.project_id.clone(), e.doc.id.clone(), e.doc.revision)
            };
            let report = crate::server::dispatch(project, "document", &serde_json::json!({"action":"save_report","project_id":project_id,"document_id":document,"expected_revision":revision})).unwrap();
            assert_eq!(report["save_allowed"], true);
            assert!(report["losses"]
                .as_array()
                .unwrap()
                .iter()
                .any(|loss| loss["code"] == "editable_source_rasterized"));
            crate::server::save_accepted_source(project, path, &document, revision, false).unwrap();
        }
        let reviewed = crate::workspace::lifecycle::update_readiness(&workspace).unwrap();
        let mut current = None;
        crate::workspace::lifecycle::publish_update_checkpoint(&workspace, &reviewed, |store| {
            capture_current_profile(&lease, store, &attempt, &initial, &mut current)
        })
        .unwrap();
        record_intent(&attempt, &initial).unwrap();
        let mut captured = None;
        crate::workspace::lifecycle::publish_update_checkpoint(&workspace, &reviewed, |store| {
            capture_final_profile(
                &lease,
                store,
                &attempt,
                &initial,
                std::process::id(),
                7,
                &reviewed,
                &mut captured,
            )
        })
        .unwrap();
        let capture = captured.as_ref().unwrap();
        let binding = &capture.binding;
        let ctx = eframe::egui::Context::default();
        let checks = driver::State::default();
        checks.begin(&ctx).unwrap();
        checks.session.borrow_mut().checking();
        checks.native_starting().unwrap();
        let mut status = coordinator::NativeStatus {
            phase: crate::sparkle::check::Phase::Quarantined,
            version: Some("0.30.1".into()),
            busy: false,
            cancelled: false,
            quarantine: true,
            native_cycle: true,
            held_generation: Some(if outcome == "newer-ready" { 8 } else { 7 }),
            found_generation: None,
            download_generation: None,
            expected_bytes: None,
            received_bytes: 0,
            extraction: None,
            download_complete: false,
            final_profile: Some(binding.clone()),
            outcome: coordinator::NativeOutcome::Unknown,
        };
        if outcome == "quarantined" {
            status.final_profile = None;
        }
        if outcome == "unsaved" {
            let mut e = projects[1].lock().unwrap();
            let layer = e
                .doc
                .layers
                .iter()
                .find(|layer| layer.source.is_none())
                .unwrap()
                .id
                .clone();
            e.edit("human", &[serde_json::json!({"op":"fill","layer":layer,"rect":[1,0,1,1],"color":[75,125,175,255]})], None, None, "Newer exit artwork").unwrap();
            e.edit("human", &[serde_json::json!({"op":"layer.update","layer":layer,"name":"Undo this exit name"})], None, None, "Keep newer redo").unwrap();
            e.undo("human").unwrap();
        }
        if outcome == "dirty-tools" {
            let mut store = tools.lock().unwrap();
            let before = store.get("brush").unwrap().profile;
            let mut after = before.clone();
            after.settings.radius += 1.;
            store.human_delta("brush", &before, &after).unwrap();
        }
        if outcome == "copy-changed" {
            fs::write(attempt.join("final-profile/tool-settings.json"), b"changed").unwrap();
        }
        if outcome == "cancel" {
            checks.cancel();
        }
        let operation = (outcome == "operation")
            .then(|| crate::server::FileOperation::begin_workspace(&workspace).unwrap());
        let before = current_workspace_bytes(&projects, &paths);
        let profile_before =
            fs::read(profile.locations.profile().join("tool-settings.json")).unwrap();
        let mut consumed = false;
        let owner_checks = std::cell::Cell::new(0);
        let result = accept_final_control(
            &checks,
            Some(&status),
            Some(binding),
            binding,
            &mut consumed,
            outcome == "blocked",
            |binding| {
                capture.finish_exit(
                    &workspace,
                    &lease,
                    &attempt,
                    &initial,
                    std::process::id(),
                    binding,
                    || {
                        owner_checks.set(owner_checks.get() + 1);
                        if outcome == "owner-gone" {
                            Err("Signed owner authentication failed".into())
                        } else {
                            Ok(())
                        }
                    },
                )
            },
        );
        assert_eq!(result.is_ok(), outcome == "success", "{outcome}");
        assert!(consumed);
        assert_eq!(
            owner_checks.get(),
            usize::from(matches!(outcome, "success" | "owner-gone"))
        );
        assert_eq!(workspace.lock().unwrap().exiting, outcome == "success");
        assert_eq!(
            tools.lock().unwrap().update_exit_committed(),
            outcome == "success"
        );
        let mut expected = before;
        if outcome == "success" {
            for row in &mut expected {
                row["closed"] = true.into();
                row["owner_path"] = serde_json::Value::Null;
                row["owner_project"] = serde_json::Value::Null;
            }
            assert!(tools.lock().unwrap().flush().is_err());
            let output = ctx.run(Default::default(), |_| checks.close_after_update_exit());
            assert!(output.viewport_output[&eframe::egui::ViewportId::ROOT]
                .commands
                .contains(&eframe::egui::ViewportCommand::Close));
        }
        assert_eq!(
            current_workspace_bytes(&projects, &paths),
            expected,
            "{outcome}"
        );
        assert_eq!(
            fs::read(profile.locations.profile().join("tool-settings.json")).unwrap(),
            profile_before
        );
        assert!(
            checks.view().native_cycle,
            "Closing must not retire the copied native owner"
        );
        assert!(require_captured_handoff().is_err());
        drop(operation);
    }
}

#[test]
fn native_death_and_callback_records_explain_recovery_without_releasing_saved_work_or_settings() {
    use super::super::coordinator::NativeOutcome;
    let f = Fixture::new();
    let profile = f.profile();
    let lease = profile.update_profile();
    let attempt = f.attempt(&lease);
    let initial = fs::read(attempt.join("checkpoint.json")).unwrap();
    fs::rename(
        attempt.join("checkpoint.json"),
        attempt.join("capture.json"),
    )
    .unwrap();
    let (workspace, projects, paths) = current_profile_workspace(&f);
    let before = current_workspace_bytes(&projects, &paths);
    let tools = workspace.lock().unwrap().tool_settings.clone();
    *tools.lock().unwrap() =
        crate::tool_settings::Store::load(profile.locations.profile().join("tool-settings.json"));
    let reviewed = crate::workspace::lifecycle::update_readiness(&workspace).unwrap();
    let mut captured = None;
    crate::workspace::lifecycle::publish_update_checkpoint(&workspace, &reviewed, |store| {
        capture_current_profile(&lease, store, &attempt, &initial, &mut captured)
    })
    .unwrap();
    record_intent(&attempt, &initial).unwrap();
    let armed = ArmedCheck {
        app: attempt.join("PeerBrush.app"),
        manifest: initial.clone(),
        intent: read_marker(&attempt.join("native-start-intent.json")).unwrap(),
        owner_pid: std::process::id(),
    };
    let settings = fs::read(profile.locations.profile().join("tool-settings.json")).unwrap();
    let installed = NativeEvent::Callback {
        outcome: NativeOutcome::Installed { relaunched: true },
    };
    assert!(armed.record_event(&lease, installed, None).is_err());
    assert!(!attempt.join(installed.marker().unwrap()).exists());
    assert!(record_native_event(
        &attempt,
        &initial,
        std::process::id() + 1,
        NativeEvent::OwnerExited,
        None
    )
    .is_err());
    assert!(!attempt.join("native-owner-exited.json").exists());
    for event in [
        NativeEvent::Callback {
            outcome: NativeOutcome::Installing {
                application_terminated: false,
            },
        },
        NativeEvent::Callback {
            outcome: NativeOutcome::Installing {
                application_terminated: true,
            },
        },
        installed,
        NativeEvent::Callback {
            outcome: NativeOutcome::Aborted,
        },
        NativeEvent::Callback {
            outcome: NativeOutcome::CycleEnded,
        },
        NativeEvent::ParentExited,
        NativeEvent::OwnerExited,
    ] {
        armed.record_event(&lease, event, Some(7)).unwrap();
        let marker = attempt.join(event.marker().unwrap());
        let bytes = read_marker(&marker).unwrap();
        armed.record_event(&lease, event, Some(7)).unwrap();
        assert_eq!(read_marker(&marker).unwrap(), bytes);
        assert!(
            armed.record_event(&lease, event, Some(8)).is_err(),
            "An observation cannot be overwritten by another generation"
        );
        assert_eq!(read_marker(&marker).unwrap(), bytes);
        assert!(startup_guard(&profile.locations).is_err());
        assert!(!workspace.lock().unwrap().exiting);
        assert!(!tools.lock().unwrap().update_exit_committed());
        assert_eq!(current_workspace_bytes(&projects, &paths), before);
        assert_eq!(
            fs::read(profile.locations.profile().join("tool-settings.json")).unwrap(),
            settings
        );
    }
    assert!(native_recovery_message(&attempt)
        .unwrap()
        .contains("owner exited"));
    let marker = attempt.join("native-owner-exited.json");
    let bytes = read_marker(&marker).unwrap();
    let mut forged: NativeRecord = serde_json::from_slice(&bytes).unwrap();
    forged.intent_sha256 = "a".repeat(64);
    fs::write(&marker, serde_json::to_vec(&forged).unwrap()).unwrap();
    assert!(native_recovery_message(&attempt).is_err());
    assert!(startup_guard(&profile.locations).is_err());
    assert!(armed
        .record_event(&lease, NativeEvent::OwnerExited, Some(7))
        .is_err());
    assert!(!attempt.join("native-not-started.json").exists());
    assert_eq!(current_workspace_bytes(&projects, &paths), before);
    assert!(require_captured_handoff().is_err());
}
