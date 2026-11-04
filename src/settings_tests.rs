use super::*;
use serde_json::json;

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("peerbrush-settings-{}", crate::engine::id()));
        fs::create_dir(&path).unwrap();
        fs::create_dir(path.join("legacy")).unwrap();
        Self(path)
    }
    fn locations(&self) -> Locations {
        Locations {
            config: self.0.join("config"),
            data: self.0.join("data"),
            cache: self.0.join("cache"),
        }
    }
    fn source(&self) -> PathBuf {
        self.0.join("legacy")
    }
    fn preferences(&self) {
        let root = self.source();
        crate::brush_library::Library::load(root.join("brushes.json"))
            .save(
                None,
                "My brush",
                "Custom",
                crate::brush::Settings::default(),
            )
            .unwrap();
        let preset = crate::filter_library::curated().remove(0);
        crate::filter_library::Library::load(root.join("filters.json"))
            .save(
                None,
                "My filter",
                "Custom",
                &preset.kind,
                preset.settings,
                0.65,
            )
            .unwrap();
        crate::production_export::library::Library::load(root.join("export-presets.json"))
            .save(
                None,
                "My export",
                crate::production_export::Settings::default(),
                0,
                "human",
            )
            .unwrap();
        let mut tools = crate::tool_settings::Store::load(root.join("tool-settings.json"));
        let mut brush = tools.get("brush").unwrap().profile;
        brush.settings.radius = 87.;
        tools.set("brush", brush, "human", None).unwrap();
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn standard_locations_respect_platform_and_absolute_xdg_overrides() {
    let home = std::env::temp_dir().join("profile-test");
    let roaming = home.join("roaming");
    let local = home.join("local");
    let xdg = home.join("xdg-config");
    let vars = |key: &str| match key {
        "HOME" => Some(home.clone().into_os_string()),
        "APPDATA" => Some(roaming.clone().into_os_string()),
        "LOCALAPPDATA" => Some(local.clone().into_os_string()),
        "XDG_CONFIG_HOME" => Some(xdg.clone().into_os_string()),
        "XDG_DATA_HOME" => Some("relative-is-ignored".into()),
        _ => None,
    };
    let win = Locations::from_environment("windows", vars).unwrap();
    assert_eq!(win.config, roaming.join("PeerBrush"));
    assert_eq!(win.data, local.join("PeerBrush/Data"));
    assert_eq!(win.cache, local.join("PeerBrush/Cache"));
    let mac = Locations::from_environment("macos", vars).unwrap();
    assert_eq!(
        mac.config,
        home.join("Library/Application Support/PeerBrush/Config")
    );
    assert_eq!(mac.cache, home.join("Library/Caches/PeerBrush"));
    let linux = Locations::from_environment("linux", vars).unwrap();
    assert_eq!(linux.config, xdg.join("peerbrush"));
    assert_eq!(linux.data, home.join(".local/share/peerbrush"));
    assert!(Locations::from_environment("windows", |_| Some("relative".into())).is_err());
}

#[test]
fn first_run_and_reinstall_keep_profile_without_copying_runtime_or_artwork() {
    let f = Fixture::new();
    f.preferences();
    for name in [
        "connection.json",
        "recoveries.json",
        "private.json",
        "painting.psd",
    ] {
        fs::write(f.source().join(name), b"private original").unwrap();
    }
    let originals: Vec<_> = FILES
        .iter()
        .filter_map(|name| {
            fs::read(f.source().join(name))
                .ok()
                .map(|bytes| (*name, bytes))
        })
        .collect();
    let profile = Profile::open(f.locations(), &f.source()).unwrap();
    let root = profile.locations.profile();
    for (name, bytes) in &originals {
        assert_eq!(fs::read(root.join(name)).unwrap(), *bytes);
        assert_eq!(
            fs::read(root.join("migration-backup").join(name)).unwrap(),
            *bytes
        );
        assert_eq!(fs::read(f.source().join(name)).unwrap(), *bytes);
    }
    assert!(!root.join("connection.json").exists());
    assert!(!root.join("painting.psd").exists());
    assert!(!root.join("private.json").exists());
    drop(profile);
    // A restart/upgrade uses the per-user profile even when the old workspace changes.
    fs::write(
        f.source().join("tool-settings.json"),
        b"older installation broken",
    )
    .unwrap();
    let profile = Profile::open(f.locations(), &f.source()).unwrap();
    let mut tools =
        crate::tool_settings::Store::load(profile.locations.profile().join("tool-settings.json"));
    assert_eq!(tools.get("brush").unwrap().profile.settings.radius, 87.);
    let mut brush = tools.get("brush").unwrap().profile;
    brush.settings.radius = 43.;
    tools.set("brush", brush, "human", None).unwrap();
    drop(profile);
    let profile = Profile::open(f.locations(), &f.source()).unwrap();
    assert_eq!(
        crate::tool_settings::Store::load(profile.locations.profile().join("tool-settings.json"))
            .get("brush")
            .unwrap()
            .profile
            .settings
            .radius,
        43.
    );
    assert_eq!(
        fs::read(f.source().join("painting.psd")).unwrap(),
        b"private original"
    );
}

#[test]
fn clean_first_run_and_explicit_workspace_preserve_existing_behavior() {
    let f = Fixture::new();
    let profile = Profile::open(f.locations(), &f.0.join("missing-legacy")).unwrap();
    assert!(profile.locations.profile().join("profile.json").is_file());
    assert!(!f.0.join("missing-legacy").exists());
    assert!(!profile.locations.data.exists());
    let parse = |args: &[&str]| {
        crate::activation::storage_arguments(args.iter().map(OsString::from), f.source()).unwrap()
    };
    assert!(!parse(&[]).2);
    assert!(parse(&["--state-dir", f.source().to_str().unwrap()]).2);
    assert!(!parse(&["--open", "--state-dir"]).2);
    assert!(!parse(&["--", "--state-dir"]).2);
}

#[test]
fn invalid_and_future_settings_fail_without_publishing_or_altering_sources() {
    for bytes in [b"invalid json".as_slice(), br#"{"version":999,"tools":{}}"#] {
        let f = Fixture::new();
        fs::write(f.source().join("tool-settings.json"), bytes).unwrap();
        assert!(Profile::open(f.locations(), &f.source()).is_err());
        assert_eq!(
            fs::read(f.source().join("tool-settings.json")).unwrap(),
            bytes
        );
        assert!(!f.locations().profile().exists());
    }
}

#[test]
fn interrupted_copy_and_source_changes_can_retry_without_partial_profile() {
    let f = Fixture::new();
    f.preferences();
    let profile = Profile::lock(f.locations()).unwrap();
    assert!(profile
        .copy_legacy(&f.source(), || Err("simulated interruption".into()))
        .is_err());
    assert!(!f.locations().profile().exists());
    // A file that was absent is also checked before publication.
    assert!(profile
        .copy_legacy(&f.source(), || {
            fs::write(
                f.source().join("shortcuts.json"),
                b"unexpected new settings",
            )
            .unwrap();
            Ok(())
        })
        .is_err());
    assert!(!f.locations().profile().exists());
    fs::remove_file(f.source().join("shortcuts.json")).unwrap();
    drop(profile);
    assert!(Profile::migrate(f.locations(), &f.source()).is_ok());
    assert!(fs::read_dir(&f.locations().config)
        .unwrap()
        .flatten()
        .any(|entry| entry
            .file_name()
            .to_string_lossy()
            .starts_with("migration-")));
}

#[test]
fn concurrent_sessions_and_existing_profiles_never_get_overwritten() {
    let f = Fixture::new();
    let source_lock = Lock::acquire(&f.source().join("instance.lock")).unwrap();
    assert!(Profile::migrate(f.locations(), &f.source()).is_err());
    drop(source_lock);
    let profile = Profile::open(f.locations(), &f.source()).unwrap();
    assert!(Profile::open(f.locations(), &f.source()).is_err());
    let manifest = fs::read(profile.locations.profile().join("profile.json")).unwrap();
    drop(profile);
    assert!(Profile::migrate(f.locations(), &f.source()).is_err());
    assert_eq!(
        fs::read(f.locations().profile().join("profile.json")).unwrap(),
        manifest
    );
    let stage = f.0.join("publish-stage");
    fs::create_dir(&stage).unwrap();
    assert!(publish_directory(&stage, &f.locations().profile()).is_err());
    assert!(stage.exists());
}

#[test]
fn incomplete_or_future_profiles_remain_protected() {
    let f = Fixture::new();
    fs::create_dir_all(f.locations().profile()).unwrap();
    assert!(Profile::open(f.locations(), &f.source()).is_err());
    fs::write(
        f.locations().profile().join("profile.json"),
        serde_json::to_vec(&json!({"version":2,"migrated_files":[]})).unwrap(),
    )
    .unwrap();
    assert!(Profile::open(f.locations(), &f.source()).is_err());
    assert!(Profile::migrate(f.locations(), &f.source()).is_err());
}

#[cfg(unix)]
#[test]
fn symbolic_legacy_files_and_profiles_are_never_followed() {
    use std::os::unix::fs::{symlink, PermissionsExt};
    let f = Fixture::new();
    fs::write(f.0.join("private"), b"do not read").unwrap();
    symlink(f.0.join("private"), f.source().join("brushes.json")).unwrap();
    assert!(Profile::open(f.locations(), &f.source()).is_err());
    assert!(!f.locations().profile().exists());
    assert_eq!(
        fs::metadata(&f.locations().config)
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o700
    );
    symlink(f.source(), f.locations().profile()).unwrap();
    assert!(Profile::open(f.locations(), &f.source()).is_err());
}

#[cfg(windows)]
#[test]
fn junction_source_and_profile_directories_remain_protected() {
    fn junction(link: &Path, target: &Path) {
        let result = std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(link)
            .arg(target)
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "Could not create isolated test junction"
        );
    }
    let f = Fixture::new();
    f.preferences();
    let linked_source = f.0.join("linked-source");
    junction(&linked_source, &f.source());
    assert!(alias(&fs::symlink_metadata(&linked_source).unwrap()));
    assert!(Profile::migrate(f.locations(), &linked_source).is_err());
    assert!(!f.locations().profile().exists());
    fs::remove_dir(&linked_source).unwrap();
    junction(&f.locations().profile(), &f.source());
    assert!(Profile::open(f.locations(), &f.source()).is_err());
    fs::remove_dir(f.locations().profile()).unwrap();
    let linked_config = f.0.join("linked-config");
    junction(&linked_config, &f.locations().config);
    let mut locations = f.locations();
    locations.config = linked_config.clone();
    assert!(Profile::open(locations, &f.source()).is_err());
    fs::remove_dir(linked_config).unwrap();
    assert!(fs::read(f.source().join("tool-settings.json")).is_ok());
}
