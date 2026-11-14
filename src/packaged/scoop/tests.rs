use super::*;
use std::fs;

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let fixture =
            Self(std::env::temp_dir().join(format!("pb-scoop user space-{}", crate::engine::id())));
        fs::create_dir_all(fixture.directory()).unwrap();
        fs::write(
            fixture.executable(),
            b"synthetic native image; never executed",
        )
        .unwrap();
        fs::write(
            fixture.directory().join("scoop-manifest.json"),
            serde_json::to_vec(&serde_json::json!({
                "version":env!("CARGO_PKG_VERSION"), "bin":"peerbrush.exe"
            }))
            .unwrap(),
        )
        .unwrap();
        fs::write(
            fixture.directory().join("scoop-install.json"),
            br#"{"architecture":"64bit","bucket":"example-local-bucket"}"#,
        )
        .unwrap();
        fixture
    }
    fn app(&self) -> PathBuf {
        self.0.join("apps/peerbrush")
    }
    fn directory(&self) -> PathBuf {
        self.app().join(env!("CARGO_PKG_VERSION"))
    }
    fn executable(&self) -> PathBuf {
        self.directory().join("peerbrush.exe")
    }
    fn current(&self) -> PathBuf {
        self.app().join("current")
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn local_package_observation_needs_actual_receipts_and_preserves_their_bytes() {
    let f = Fixture::new();
    let manifest = f.directory().join("scoop-manifest.json");
    let install = f.directory().join("scoop-install.json");
    let original = [fs::read(&manifest).unwrap(), fs::read(&install).unwrap()];
    assert_eq!(owner(Some(&f.executable())), Owner::ScoopPackage);
    assert!(
        command(&f.executable()).is_err(),
        "No stable current launcher yet"
    );
    assert_eq!(fs::read(&manifest).unwrap(), original[0]);
    assert_eq!(fs::read(&install).unwrap(), original[1]);
    fs::remove_file(&install).unwrap();
    assert_eq!(owner(Some(&f.executable())), Owner::Unknown);
    assert!(command(&f.executable()).is_err());
    assert_eq!(fs::read(&manifest).unwrap(), original[0]);
    assert_eq!(owner(None), Owner::Unknown);
    let portable = f.0.join("peerbrush.exe");
    fs::write(&portable, b"ordinary portable image").unwrap();
    assert_eq!(owner(Some(&portable)), Owner::Direct);
    assert_eq!(command(&portable).unwrap(), None);
    assert!(!f.current().exists());
}

#[test]
fn legacy_utf8_bom_receipts_work_but_invalid_preferred_records_cannot_fall_back() {
    let f = Fixture::new();
    for (preferred, legacy) in [
        ("scoop-manifest.json", "manifest.json"),
        ("scoop-install.json", "install.json"),
    ] {
        let path = f.directory().join(preferred);
        let mut data = b"\xef\xbb\xbf".to_vec();
        data.extend(fs::read(&path).unwrap());
        fs::write(f.directory().join(legacy), data).unwrap();
        fs::remove_file(path).unwrap();
    }
    assert_eq!(owner(Some(&f.executable())), Owner::ScoopPackage);
    fs::write(f.directory().join("scoop-manifest.json"), b"{}").unwrap();
    assert_eq!(owner(Some(&f.executable())), Owner::Unknown);
    assert!(command(&f.executable()).is_err());
}

#[test]
fn wrong_versions_architectures_duplicate_fields_and_oversized_records_remain_unknown() {
    let f = Fixture::new();
    let manifest = f.directory().join("scoop-manifest.json");
    let install = f.directory().join("scoop-install.json");
    let original = fs::read(&manifest).unwrap();
    for bytes in [
        br#"{"version":"0.0.0","bin":"peerbrush.exe"}"#.to_vec(),
        format!(
            r#"{{"version":"{}","bin":"other.exe"}}"#,
            env!("CARGO_PKG_VERSION")
        )
        .into_bytes(),
        format!(
            r#"{{"version":"0.0.0","version":"{}","bin":"peerbrush.exe"}}"#,
            env!("CARGO_PKG_VERSION")
        )
        .into_bytes(),
        vec![b' '; RECEIPT_LIMIT as usize + 1],
    ] {
        fs::write(&manifest, &bytes).unwrap();
        assert_eq!(owner(Some(&f.executable())), Owner::Unknown);
        assert!(command(&f.executable()).is_err());
        assert_eq!(fs::read(&manifest).unwrap(), bytes);
    }
    fs::write(&manifest, original).unwrap();
    for bytes in [
        br#"{"architecture":"arm64"}"#.as_slice(),
        br#"{"architecture":"32bit"}"#,
        br#"{"architecture":"arm64","architecture":"64bit"}"#,
    ] {
        fs::write(&install, bytes).unwrap();
        assert_eq!(owner(Some(&f.executable())), Owner::Unknown);
        assert!(command(&f.executable()).is_err());
    }
}

fn link_directory(target: &Path, link: &Path) {
    #[cfg(unix)]
    std::os::unix::fs::symlink(target, link).unwrap();
    #[cfg(windows)]
    assert!(
        std::process::Command::new("cmd")
            .args(["/D", "/C", "mklink", "/J"])
            .arg(link)
            .arg(target)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .unwrap()
            .success(),
        "Create only an isolated fixture junction"
    );
}
fn unlink_directory(link: &Path) {
    #[cfg(unix)]
    fs::remove_file(link).unwrap();
    #[cfg(windows)]
    fs::remove_dir(link).unwrap();
}

#[test]
fn current_launcher_follows_versions_and_refuses_a_stale_running_build() {
    let f = Fixture::new();
    let expected = f
        .app()
        .canonicalize()
        .unwrap()
        .join("current")
        .join("peerbrush.exe");
    link_directory(&f.directory(), &f.current());
    assert_eq!(command(&f.executable()).unwrap(), Some(expected.clone()));
    assert_eq!(
        command(&f.current().join("peerbrush.exe")).unwrap(),
        Some(expected)
    );
    let next = f.app().join("999.0.0");
    fs::create_dir(&next).unwrap();
    fs::write(
        next.join("peerbrush.exe"),
        b"different version; never executed",
    )
    .unwrap();
    unlink_directory(&f.current());
    link_directory(&next, &f.current());
    assert_eq!(owner(Some(&f.executable())), Owner::ScoopPackage);
    assert!(
        command(&f.executable()).is_err(),
        "Never pin the old version when current changes"
    );
    unlink_directory(&f.current());
    assert!(command(&f.executable()).is_err());
    assert!(f.executable().is_file());
}

#[cfg(unix)]
#[test]
fn linked_and_special_receipts_are_refused_before_reading_their_contents() {
    use std::{ffi::CString, os::unix::ffi::OsStrExt};
    let f = Fixture::new();
    let receipt = f.directory().join("scoop-install.json");
    let target = f.0.join("unrelated.json");
    fs::rename(&receipt, &target).unwrap();
    std::os::unix::fs::symlink(&target, &receipt).unwrap();
    assert_eq!(owner(Some(&f.executable())), Owner::Unknown);
    fs::remove_file(&receipt).unwrap();
    let name = CString::new(receipt.as_os_str().as_bytes()).unwrap();
    assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
    assert_eq!(owner(Some(&f.executable())), Owner::Unknown);
    assert_eq!(
        fs::read(target).unwrap(),
        br#"{"architecture":"64bit","bucket":"example-local-bucket"}"#
    );
}
