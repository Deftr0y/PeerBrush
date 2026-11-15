use super::*;

#[test]
fn prefix_hints_are_bounded_locations_without_parent_escapes() {
    let root = std::env::temp_dir().join("brew prefix with spaces");
    let executable = root.join("bin").join("peerbrush");
    let paths = candidates(&executable, Some(&root));
    assert_eq!(paths.iter().filter(|path| **path == root).count(), 1);
    let relative = Path::new("relative/prefix");
    assert!(!candidates(Path::new("peerbrush"), Some(relative)).contains(&relative.to_path_buf()));
    let escape = root.join("..").join("other");
    assert!(!candidates(&executable, Some(&escape)).contains(&escape));
    assert_eq!(owner(None), Owner::Unknown);
    for owner in [
        Owner::HomebrewPackage,
        Owner::NixPackage,
        Owner::ScoopPackage,
        Owner::Unknown,
    ] {
        assert!(require_direct(owner).is_err());
    }
    assert!(require_direct(Owner::Direct).is_ok());
}

#[cfg(unix)]
mod native_paths {
    use super::*;
    use std::os::unix::fs::{symlink, PermissionsExt};

    struct Fixture {
        root: PathBuf,
        prefix: PathBuf,
        app: PathBuf,
    }
    impl Fixture {
        fn new() -> Self {
            let root =
                std::env::temp_dir().join(format!("pb-brew paths 作品-{}", crate::engine::id()));
            fs::create_dir(&root).unwrap();
            let root = root.canonicalize().unwrap();
            let f = Self {
                prefix: root.join("brew prefix"),
                app: root.join("application folder/PeerBrush.app"),
                root,
            };
            fs::create_dir_all(f.executable().parent().unwrap()).unwrap();
            fs::write(f.executable(), b"synthetic native image; never executed").unwrap();
            fs::set_permissions(f.executable(), fs::Permissions::from_mode(0o755)).unwrap();
            f.install_prefix(&f.prefix, env!("CARGO_PKG_VERSION"));
            fs::write(
                f.root.join("artwork.psd"),
                b"human document outside installation",
            )
            .unwrap();
            fs::write(
                f.root.join("settings.json"),
                b"human settings outside installation",
            )
            .unwrap();
            f
        }
        fn executable(&self) -> PathBuf {
            self.app.join("Contents/MacOS/peerbrush")
        }
        fn record(&self) -> PathBuf {
            self.prefix
                .join("Caskroom/peerbrush/.metadata/INSTALL_RECEIPT.json")
        }
        fn launcher(&self) -> PathBuf {
            self.prefix.join("bin/peerbrush")
        }
        fn install_prefix(&self, prefix: &Path, version: &str) {
            let record = prefix.join("Caskroom/peerbrush/.metadata/INSTALL_RECEIPT.json");
            fs::create_dir_all(record.parent().unwrap()).unwrap();
            fs::write(record, serde_json::to_vec(&serde_json::json!({
                "arch":"arm64", "source":{"version":version, "tap":"example/tap", "path":"DO_NOT_LEAK_LOCAL_CASK"}
            })).unwrap()).unwrap();
            let source = prefix.join(format!(
                "Caskroom/peerbrush/{version}/PeerBrush-{version}-macOS-arm64/PeerBrush.app"
            ));
            fs::create_dir_all(source.parent().unwrap()).unwrap();
            symlink(&self.app, source).unwrap();
            fs::create_dir_all(prefix.join("bin")).unwrap();
            symlink(self.executable(), prefix.join("bin/peerbrush")).unwrap();
        }
        fn inspect(&self) -> Result<Option<Installation>, String> {
            observe(&self.executable(), &[self.prefix.clone()])
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    #[test]
    fn moved_app_and_receipt_bind_owner_launcher_and_replacement_refusal_without_mutation() {
        let f = Fixture::new();
        let record = fs::read(f.record()).unwrap();
        let image = fs::read(f.executable()).unwrap();
        assert_eq!(classify(f.inspect()), Owner::HomebrewPackage);
        assert!(require_direct(classify(f.inspect())).is_err());
        let installed = f.inspect().unwrap().unwrap();
        assert_eq!(installed.executable, f.executable());
        assert_eq!(select(installed).unwrap(), f.launcher());
        assert_eq!(fs::read(f.record()).unwrap(), record);
        assert_eq!(fs::read(f.executable()).unwrap(), image);
        assert_eq!(
            fs::read(f.root.join("artwork.psd")).unwrap(),
            b"human document outside installation"
        );
        assert_eq!(
            fs::read(f.root.join("settings.json")).unwrap(),
            b"human settings outside installation"
        );
        let direct = f
            .root
            .join("direct copy/PeerBrush.app/Contents/MacOS/peerbrush");
        fs::create_dir_all(direct.parent().unwrap()).unwrap();
        fs::write(&direct, &image).unwrap();
        assert_eq!(
            classify(observe(&direct, &[f.prefix.clone()])),
            Owner::Direct
        );
        assert!(require_direct(classify(observe(&direct, &[f.prefix.clone()]))).is_ok());
    }

    #[test]
    fn root_disk_image_slot_requires_a_unique_link_and_current_receipt() {
        let f = Fixture::new();
        let version = env!("CARGO_PKG_VERSION");
        let staged = f.prefix.join("Caskroom/peerbrush").join(version);
        let zip = staged.join(format!("PeerBrush-{version}-macOS-arm64/PeerBrush.app"));
        let dmg = staged.join("PeerBrush.app");
        symlink(&f.app, &dmg).unwrap();
        assert_eq!(classify(f.inspect()), Owner::Unknown);
        fs::remove_file(zip).unwrap();
        assert_eq!(classify(f.inspect()), Owner::HomebrewPackage);
        assert_eq!(select(f.inspect().unwrap().unwrap()).unwrap(), f.launcher());
        fs::remove_file(f.record()).unwrap();
        assert_eq!(classify(f.inspect()), Owner::Unknown);
        assert!(require_direct(classify(f.inspect())).is_err());
    }

    #[test]
    fn missing_stale_or_inconsistent_receipts_hold_instead_of_becoming_direct_updates() {
        let f = Fixture::new();
        let original = fs::read(f.record()).unwrap();
        fs::remove_file(f.record()).unwrap();
        assert_eq!(classify(f.inspect()), Owner::Unknown);
        assert!(f.inspect().is_err());
        assert!(!f.record().exists());
        for bytes in [
            br#"{"arch":"arm64","source":{"version":"../escape"}}"#.to_vec(),
            format!(
                r#"{{"arch":"x86_64","source":{{"version":"{}"}}}}"#,
                env!("CARGO_PKG_VERSION")
            )
            .into_bytes(),
            format!(
                r#"{{"arch":"arm64","arch":"arm64","source":{{"version":"{}"}}}}"#,
                env!("CARGO_PKG_VERSION")
            )
            .into_bytes(),
            format!(
                r#"{{"arch":"arm64","source":{{"version":"0.0.0","version":"{}"}}}}"#,
                env!("CARGO_PKG_VERSION")
            )
            .into_bytes(),
            vec![b' '; LIMIT as usize + 1],
        ] {
            fs::write(f.record(), &bytes).unwrap();
            let error = f.inspect().err().expect("Invalid records stay uncertain");
            assert!(!error.contains("DO_NOT_LEAK"));
            assert_eq!(classify(f.inspect()), Owner::Unknown);
            assert!(require_direct(classify(f.inspect())).is_err());
            assert_eq!(fs::read(f.record()).unwrap(), bytes);
        }
        fs::write(f.record(), original).unwrap();
        assert_eq!(classify(f.inspect()), Owner::HomebrewPackage);
        // Homebrew has updated its receipt and app pointer before this older process exits.
        let next = "999.0.0";
        let next_slot = f.prefix.join(format!(
            "Caskroom/peerbrush/{next}/PeerBrush-{next}-macOS-arm64/PeerBrush.app"
        ));
        fs::create_dir_all(next_slot.parent().unwrap()).unwrap();
        symlink(&f.app, next_slot).unwrap();
        fs::write(
            f.record(),
            format!(r#"{{"arch":"arm64","source":{{"version":"{next}"}}}}"#),
        )
        .unwrap();
        assert_eq!(classify(f.inspect()), Owner::Unknown);
    }

    #[test]
    fn missing_non_executable_foreign_and_retargeted_launchers_are_refused() {
        let f = Fixture::new();
        let installed = f.inspect().unwrap().unwrap();
        fs::remove_file(f.launcher()).unwrap();
        assert!(select(installed).is_err());
        assert_eq!(classify(f.inspect()), Owner::HomebrewPackage);
        let other = f.root.join("other executable");
        fs::write(&other, b"other build").unwrap();
        symlink(&other, f.launcher()).unwrap();
        assert!(select(f.inspect().unwrap().unwrap()).is_err());
        fs::remove_file(f.launcher()).unwrap();
        symlink(f.executable(), f.launcher()).unwrap();
        fs::set_permissions(f.executable(), fs::Permissions::from_mode(0o644)).unwrap();
        assert!(select(f.inspect().unwrap().unwrap()).is_err());
        fs::set_permissions(f.executable(), fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(select(f.inspect().unwrap().unwrap()).unwrap(), f.launcher());
    }

    #[test]
    fn two_prefixes_claiming_the_same_app_remain_ambiguous() {
        let f = Fixture::new();
        let second = f.root.join("other prefix");
        f.install_prefix(&second, env!("CARGO_PKG_VERSION"));
        let observed = observe(&f.executable(), &[f.prefix.clone(), second]);
        assert_eq!(classify(observed), Owner::Unknown);
    }

    #[test]
    fn linked_and_special_receipts_never_gain_ownership_or_block_on_reads() {
        use std::{ffi::CString, os::unix::ffi::OsStrExt};
        let f = Fixture::new();
        let saved = f.root.join("saved receipt.json");
        fs::rename(f.record(), &saved).unwrap();
        symlink(&saved, f.record()).unwrap();
        assert_eq!(classify(f.inspect()), Owner::Unknown);
        fs::remove_file(f.record()).unwrap();
        let name = CString::new(f.record().as_os_str().as_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
        assert_eq!(classify(f.inspect()), Owner::Unknown);
        assert!(fs::read_to_string(saved)
            .unwrap()
            .contains("DO_NOT_LEAK_LOCAL_CASK"));
    }
}
