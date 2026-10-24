use super::*;
use std::{collections::BTreeMap, io::Cursor, sync::atomic::AtomicUsize};

struct Temp(PathBuf);
impl Temp {
    fn new() -> Self {
        let path =
            std::env::temp_dir().join(format!("pb-windows-prepared-{}", crate::engine::id()));
        fs::create_dir(&path).unwrap();
        Self(fs::canonicalize(path).unwrap())
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        if fs::symlink_metadata(&self.0).is_ok_and(|m| m.is_dir() && !m.file_type().is_symlink())
            && fs::canonicalize(&self.0).is_ok_and(|p| p == self.0)
        {
            fs::remove_dir_all(&self.0).unwrap();
        }
    }
}
pub(super) fn policy() -> Policy {
    Policy {
        discovery: "https://example.invalid/stable/PeerBrush-Windows-x64.appinstaller".into(),
        archive_root: "https://example.invalid/windows/".into(),
        name: "PeerBrush.Synthetic".into(),
        publisher: "CN=Public Synthetic".into(),
        publisher_display: "Synthetic Publisher".into(),
        family: "PeerBrush.Synthetic_abc123".into(),
        certificate: "a".repeat(64),
        runtime_uri: "https://example.invalid/windows/runtime.appx".into(),
        runtime_name: RUNTIME_NAME.into(),
        runtime_publisher: RUNTIME_PUBLISHER.into(),
        runtime_certificate: "b".repeat(64),
    }
}
fn source() -> Source {
    Source {
        full_name: "PeerBrush.Synthetic_1.30.0.0_x64__abc123".into(),
        family: policy().family,
        version: [1, 30, 0, 0],
        association: "https://example.invalid/windows/PeerBrush-1.30.0.0-Windows-x64.appinstaller"
            .into(),
        build: "1".repeat(40),
    }
}
fn selected_target() -> Target {
    Target {
        full_name: "PeerBrush.Synthetic_1.30.1.0_x64__abc123".into(),
        version: [1, 30, 1, 0],
        build: "2".repeat(40),
        executable: Media {
            bytes: 26,
            sha256: hash(b"selected-native-executable"),
        },
        source_name: "PeerBrush-0.30.1-source.zip".into(),
        source: Media {
            bytes: 22,
            sha256: hash(b"selected-native-source"),
        },
    }
}
fn new_descriptor() -> String {
    "https://example.invalid/windows/PeerBrush-1.30.1.0-Windows-x64.appinstaller".into()
}
fn xml() -> Vec<u8> {
    let p = policy();
    format!("<?xml version=\"1.0\" encoding=\"utf-8\"?><AppInstaller xmlns=\"http://schemas.microsoft.com/appx/appinstaller/2017\" Version=\"1.30.1.0\" Uri=\"{}\"><MainPackage Name=\"{}\" Publisher=\"{}\" Version=\"1.30.1.0\" ProcessorArchitecture=\"x64\" Uri=\"https://example.invalid/windows/PeerBrush-1.30.1.0.msix\"/><Dependencies><Package Name=\"{}\" Publisher=\"{}\" Version=\"14.0.30704.0\" ProcessorArchitecture=\"x64\" Uri=\"{}\"/></Dependencies></AppInstaller>", new_descriptor(), p.name, p.publisher, p.runtime_name, p.runtime_publisher, p.runtime_uri).into_bytes()
}
fn control() -> Control {
    Control {
        cancelled: Arc::new(AtomicBool::new(false)),
        deadline: Instant::now() + TIMEOUT,
        view: Arc::new(Mutex::new(View::default())),
    }
}
struct HttpFake {
    files: BTreeMap<String, (Vec<u8>, u64)>,
    calls: Mutex<Vec<String>>,
}
impl HttpFake {
    fn new() -> Self {
        let files = [
            (source().association, xml()),
            (policy().discovery, xml()),
            (new_descriptor(), xml()),
            (
                "https://example.invalid/windows/PeerBrush-1.30.1.0.msix".into(),
                b"main-native-bytes".to_vec(),
            ),
            (policy().runtime_uri, b"runtime-native-bytes".to_vec()),
        ]
        .into_iter()
        .map(|(k, v)| {
            let n = v.len() as u64;
            (k, (v, n))
        })
        .collect();
        Self {
            files,
            calls: Mutex::new(vec![]),
        }
    }
}
impl Transport for HttpFake {
    fn get(&self, uri: &str) -> Result<Response> {
        self.calls.lock().unwrap().push(uri.into());
        let (data, length) = self
            .files
            .get(uri)
            .ok_or("Synthetic transport rejected URI.")?;
        Ok(Response {
            reader: Box::new(Cursor::new(data.clone())),
            length: *length,
        })
    }
}
struct NativeFake {
    changes: bool,
    reject: bool,
    source_calls: AtomicUsize,
    media_calls: AtomicUsize,
}
impl NativeFake {
    fn new() -> Self {
        Self {
            changes: false,
            reject: false,
            source_calls: AtomicUsize::new(0),
            media_calls: AtomicUsize::new(0),
        }
    }
}
impl Verifier for NativeFake {
    fn source(&self, _: &Policy) -> Result<Source> {
        let mut source = source();
        if self.source_calls.fetch_add(1, Ordering::Relaxed) > 0 && self.changes {
            source.full_name.push_str("-changed");
        }
        Ok(source)
    }
    fn media(
        &self,
        file: &Held,
        _: &Policy,
        _: &Selection,
        runtime: bool,
        control: &Control,
    ) -> Result<Option<Target>> {
        self.media_calls.fetch_add(1, Ordering::Relaxed);
        control.check()?;
        file.check(control)?;
        let expected = if runtime {
            b"runtime-native-bytes".as_slice()
        } else {
            b"main-native-bytes".as_slice()
        };
        if self.reject || file.media.sha256 != hash(expected) {
            return Err("Synthetic native signature or payload rejected actual bytes.".into());
        }
        Ok((!runtime).then(selected_target))
    }
}
pub(super) fn receipt() -> Prepared {
    Prepared {
        schema: 1,
        source: source(),
        policy: policy(),
        selection: descriptor(&xml(), &policy(), &source()).unwrap(),
        descriptor: Media {
            bytes: xml().len() as u64,
            sha256: hash(&xml()),
        },
        package: Media {
            bytes: 17,
            sha256: hash(b"main-native-bytes"),
        },
        runtime: Media {
            bytes: 20,
            sha256: hash(b"runtime-native-bytes"),
        },
    }
}

#[test]
fn strict_descriptor_rejects_downgrade_extra_settings_foreign_identity_and_uri_before_media() {
    let original = String::from_utf8(xml()).unwrap();
    assert_eq!(
        descriptor(original.as_bytes(), &policy(), &source())
            .unwrap()
            .version,
        [1, 30, 1, 0]
    );
    for bad in [
        original.replace("1.30.1.0", "1.30.0.0"),
        original.replace("MainPackage Name", "MainPackage Extra=\"true\" Name"),
        original.replace("</AppInstaller>", "<UpdateSettings/></AppInstaller>"),
        original.replace("windows/PeerBrush", "other/PeerBrush"),
        original.replace(
            "ProcessorArchitecture=\"x64\"",
            "ProcessorArchitecture=\"arm64\"",
        ),
        original.replace("CN=Public Synthetic", "CN=Someone Else"),
        original.replace("runtime.appx", "runtime.appx?token=secret"),
        original.replace("1.30.1.0", "1.030.1.0"),
        original.replace("<Dependencies>", "<Dependencies>unexpected"),
        original.replace("<MainPackage", "<Other xmlns=\"urn:foreign\"/><MainPackage"),
    ] {
        assert!(descriptor(bad.as_bytes(), &policy(), &source()).is_err());
    }
    let mut p = policy();
    p.certificate = "REPLACE_CERTIFICATE".into();
    assert!(descriptor(&xml(), &p, &source()).is_err());
    let mut s = source();
    s.build = "unknown".into();
    assert!(descriptor(&xml(), &policy(), &s).is_err());
}

#[test]
fn current_association_follows_the_actual_version_without_changing_distribution_policy() {
    let temp = Temp::new();
    let root = temp.0.join("windows-prepared");
    let native = RunningNative::new(&temp.0.join("installed"));
    let http = HttpFake::new();
    capture_current_only(&root, &policy(), &http, &native, &control()).unwrap();
    assert_eq!(
        *http.calls.lock().unwrap(),
        vec![
            new_descriptor(),
            "https://example.invalid/windows/PeerBrush-1.30.1.0.msix".into(),
            policy().runtime_uri,
        ]
    );
    assert_eq!(native.current_media_calls.load(Ordering::Relaxed), 1);
    assert_eq!(native.new_main_calls.load(Ordering::Relaxed), 0);
    let retained = read_prepared_for(
        &captured_current(&root),
        &policy(),
        &control(),
        MediaRole::Current,
    )
    .unwrap();
    assert_eq!(retained.2.source, native.current);
    assert_eq!(retained.2.selection.descriptor, new_descriptor());
    assert!(!ENABLED && configured().is_none());
}

#[test]
fn invalid_installed_associations_refuse_before_network_or_native_media() {
    for association in [
        source().association,
        policy().discovery,
        new_descriptor().replace("example.invalid", "foreign.invalid"),
        new_descriptor().replace("/windows/", "/elsewhere/"),
        new_descriptor().replace("1.30.1.0", "1.030.1.0"),
        new_descriptor() + "?token=private",
        new_descriptor().replace("https:", "http:"),
    ] {
        let temp = Temp::new();
        let mut native = RunningNative::new(&temp.0.join("installed"));
        native.current.association = association;
        let http = HttpFake::new();
        assert!(capture_current_only(
            &temp.0.join("windows-prepared"),
            &policy(),
            &http,
            &native,
            &control(),
        )
        .is_err());
        assert!(http.calls.lock().unwrap().is_empty());
        assert_eq!(native.current_media_calls.load(Ordering::Relaxed), 0);
        assert_eq!(native.new_main_calls.load(Ordering::Relaxed), 0);
    }
}

#[test]
fn selected_running_association_must_equal_the_retained_immutable_descriptor() {
    let temp = Temp::new();
    let cache = temp.0.join("windows-prepared");
    let attempt = completed_stage(&cache);
    let original = fs::read(attempt.join("prepared.json")).unwrap();
    let outcome = fs::read(attempt.join("stage-outcome.json")).unwrap();
    let mut native = RunningNative::new(&temp.0.join("installed"));
    native.current.association = new_descriptor().replace("/windows/", "/windows/other/");
    assert!(policy().source_descriptor(&native.current).is_ok());
    assert!(inspect_retained(&cache, &policy(), &native, &control()).is_err());
    assert_eq!(fs::read(attempt.join("prepared.json")).unwrap(), original);
    assert_eq!(
        fs::read(attempt.join("stage-outcome.json")).unwrap(),
        outcome
    );
    native.current.association = new_descriptor();
    assert!(matches!(
        inspect_retained(&cache, &policy(), &native, &control()).unwrap(),
        Reconciled::SelectedTargetRunning([1, 30, 1, 0])
    ));
}

#[test]
fn fresh_publication_authenticates_its_new_marker_and_reopen_rejects_restored_stamp_rewrite() {
    let temp = Temp::new();
    assert!(!temp.0.join("prepared.json").exists());
    let prepared = prepare(
        &temp.0,
        &policy(),
        &HttpFake::new(),
        &NativeFake::new(),
        &control(),
    )
    .unwrap();
    assert_eq!(prepared, receipt());
    let marker = temp.0.join("prepared.json");
    let before = fs::metadata(&marker).unwrap();
    let identity = crate::file_safety::identity(&File::open(&marker).unwrap()).unwrap();
    let bytes = fs::read(&marker).unwrap();
    let changed =
        String::from_utf8(bytes.clone())
            .unwrap()
            .replacen("\"schema\":1", "\"schema\":2", 1);
    assert_ne!(changed.as_bytes(), bytes);
    assert_eq!(changed.len(), bytes.len());
    fs::write(&marker, changed.as_bytes()).unwrap();
    let rewritten = File::options().write(true).open(&marker).unwrap();
    rewritten
        .set_times(fs::FileTimes::new().set_modified(before.modified().unwrap()))
        .unwrap();
    drop(rewritten);
    assert_eq!(
        crate::file_safety::identity(&File::open(&marker).unwrap()).unwrap(),
        identity
    );
    assert_eq!(fs::metadata(&marker).unwrap().len(), before.len());
    assert_eq!(
        fs::metadata(&marker).unwrap().modified().unwrap(),
        before.modified().unwrap()
    );
    assert!(reconcile(&temp.0, &policy(), &NativeFake::new(), &control()).is_err());
    assert_eq!(fs::read(&marker).unwrap(), changed.as_bytes());
}

#[test]
fn actual_streams_publish_only_verified_bytes_and_reopen_revalidates_without_network() {
    let temp = Temp::new();
    let transport = HttpFake::new();
    let native = NativeFake::new();
    let prepared = prepare(&temp.0, &policy(), &transport, &native, &control()).unwrap();
    assert_eq!(transport.calls.lock().unwrap().len(), 4);
    assert_eq!(native.media_calls.load(Ordering::Relaxed), 2);
    assert_eq!(prepared.package.sha256, hash(b"main-native-bytes"));
    assert_eq!(
        fs::read(temp.0.join("main.msix")).unwrap(),
        b"main-native-bytes"
    );
    let journal = fs::read(temp.0.join("prepared.json")).unwrap();
    let no_network = HttpFake {
        files: BTreeMap::new(),
        calls: Mutex::new(vec![]),
    };
    assert_eq!(
        prepare(
            &temp.0,
            &policy(),
            &no_network,
            &NativeFake::new(),
            &control()
        )
        .unwrap(),
        prepared
    );
    assert!(no_network.calls.lock().unwrap().is_empty());
    assert_eq!(fs::read(temp.0.join("prepared.json")).unwrap(), journal);
    let path = temp.0.join("main.msix");
    let before = fs::metadata(&path).unwrap();
    fs::write(&path, b"evil-native-bytes").unwrap();
    File::options()
        .write(true)
        .open(&path)
        .unwrap()
        .set_times(fs::FileTimes::new().set_modified(before.modified().unwrap()))
        .unwrap();
    assert_eq!(fs::metadata(&path).unwrap().len(), before.len());
    assert!(
        reconcile(&temp.0, &policy(), &NativeFake::new(), &control())
            .unwrap_err()
            .contains("media changed")
    );
    assert_eq!(
        fs::read(temp.0.join("prepared.json")).unwrap(),
        journal,
        "No corrupted-cache self certification"
    );
}

#[test]
fn rejected_native_media_changed_source_and_changed_policy_never_publish_prepared() {
    for (changes, reject) in [(true, false), (false, true)] {
        let temp = Temp::new();
        let mut native = NativeFake::new();
        native.changes = changes;
        native.reject = reject;
        assert!(prepare(&temp.0, &policy(), &HttpFake::new(), &native, &control()).is_err());
        assert!(!temp.0.join("prepared.json").exists());
    }
    let temp = Temp::new();
    prepare(
        &temp.0,
        &policy(),
        &HttpFake::new(),
        &NativeFake::new(),
        &control(),
    )
    .unwrap();
    let before = fs::read(temp.0.join("prepared.json")).unwrap();
    let mut changed = policy();
    changed.certificate = "c".repeat(64);
    assert!(reconcile(&temp.0, &changed, &NativeFake::new(), &control()).is_err());
    let mut value: serde_json::Value = serde_json::from_slice(&before).unwrap();
    value["verified"] = true.into();
    fs::write(
        temp.0.join("prepared.json"),
        serde_json::to_vec(&value).unwrap(),
    )
    .unwrap();
    assert!(
        reconcile(&temp.0, &policy(), &NativeFake::new(), &control())
            .unwrap_err()
            .contains("metadata is invalid")
    );
}

#[test]
fn truncated_extra_oversized_and_canceled_streams_remain_unprepared_and_preserved() {
    for adjustment in [-1, 1, 2] {
        let temp = Temp::new();
        let mut transport = HttpFake::new();
        let (_, declared) = transport.files.get_mut(&new_descriptor()).unwrap();
        *declared = match adjustment {
            -1 => *declared - 1,
            1 => *declared + 1,
            _ => SMALL + 1,
        };
        assert!(prepare(
            &temp.0,
            &policy(),
            &transport,
            &NativeFake::new(),
            &control()
        )
        .is_err());
        assert!(!temp.0.join("prepared.json").exists());
    }
    let temp = Temp::new();
    let c = control();
    struct CancelRead {
        canceled: Arc<AtomicBool>,
        input: Cursor<Vec<u8>>,
    }
    impl Read for CancelRead {
        fn read(&mut self, bytes: &mut [u8]) -> std::io::Result<usize> {
            let n = self.input.read(bytes)?;
            self.canceled.store(true, Ordering::Release);
            Ok(n)
        }
    }
    struct CancelHttp(Arc<AtomicBool>);
    impl Transport for CancelHttp {
        fn get(&self, _: &str) -> Result<Response> {
            Ok(Response {
                reader: Box::new(CancelRead {
                    canceled: self.0.clone(),
                    input: Cursor::new(xml()),
                }),
                length: xml().len() as u64,
            })
        }
    }
    assert!(prepare(
        &temp.0,
        &policy(),
        &CancelHttp(c.cancelled.clone()),
        &NativeFake::new(),
        &c
    )
    .unwrap_err()
    .contains("canceled"));
    assert!(!temp.0.join("prepared.json").exists());
    let retained = fs::read(temp.0.join("discovery.appinstaller")).unwrap();
    let transport = HttpFake::new();
    assert!(prepare(
        &temp.0,
        &policy(),
        &transport,
        &NativeFake::new(),
        &control()
    )
    .unwrap_err()
    .contains("interrupted"));
    assert!(transport.calls.lock().unwrap().is_empty());
    assert_eq!(
        fs::read(temp.0.join("discovery.appinstaller")).unwrap(),
        retained
    );
}

#[test]
fn interrupted_acquisition_retries_only_in_fresh_owned_attempt_and_reopens_without_get() {
    let temp = Temp::new();
    let mut interrupted = HttpFake::new();
    interrupted.files.get_mut(&new_descriptor()).unwrap().1 += 1;
    assert!(acquire(
        &temp.0,
        &policy(),
        &interrupted,
        &NativeFake::new(),
        &control()
    )
    .is_err());
    let attempts = || {
        fs::read_dir(&temp.0)
            .unwrap()
            .map(|e| e.unwrap().path())
            .filter(|p| p.file_name().unwrap() != ".peerbrush-versions")
            .collect::<Vec<_>>()
    };
    let original = attempts();
    assert_eq!(original.len(), 1);
    assert!(!original[0].join("prepared.json").exists());
    let retained = fs::read(original[0].join("selected.appinstaller")).unwrap();
    assert_eq!(retained, xml());
    let transport = HttpFake::new();
    let prepared = acquire(
        &temp.0,
        &policy(),
        &transport,
        &NativeFake::new(),
        &control(),
    )
    .unwrap();
    assert_eq!(transport.calls.lock().unwrap().len(), 4);
    assert_eq!(attempts().len(), 2);
    assert_eq!(
        fs::read(original[0].join("selected.appinstaller")).unwrap(),
        retained
    );
    assert!(!original[0].join("prepared.json").exists());
    let completed = attempts()
        .into_iter()
        .find(|p| p.join("prepared.json").exists())
        .unwrap();
    let marker = fs::read(completed.join("prepared.json")).unwrap();
    let no_network = HttpFake {
        files: BTreeMap::new(),
        calls: Mutex::new(vec![]),
    };
    let native = NativeFake::new();
    assert_eq!(
        acquire(&temp.0, &policy(), &no_network, &native, &control()).unwrap(),
        prepared
    );
    assert!(no_network.calls.lock().unwrap().is_empty());
    assert_eq!(native.media_calls.load(Ordering::Relaxed), 2);
    assert_eq!(fs::read(completed.join("prepared.json")).unwrap(), marker);
    assert_eq!(
        fs::read(original[0].join("selected.appinstaller")).unwrap(),
        retained
    );

    let competing = temp.0.join(format!("attempt-{}", crate::engine::id()));
    fs::create_dir(&competing).unwrap();
    fs::write(competing.join("prepared.json"), &marker).unwrap();
    assert!(acquire(
        &temp.0,
        &policy(),
        &no_network,
        &NativeFake::new(),
        &control()
    )
    .unwrap_err()
    .contains("ambiguous"));
    assert!(no_network.calls.lock().unwrap().is_empty());
    assert_eq!(fs::read(completed.join("prepared.json")).unwrap(), marker);
}

#[test]
fn retained_attempt_budget_and_real_acquisition_lease_refuse_without_deleting_or_get() {
    let temp = Temp::new();
    let mut interrupted = HttpFake::new();
    interrupted.files.get_mut(&new_descriptor()).unwrap().1 += 1;
    for _ in 0..3 {
        assert!(acquire(
            &temp.0,
            &policy(),
            &interrupted,
            &NativeFake::new(),
            &control()
        )
        .is_err());
    }
    let retained = fs::read_dir(&temp.0)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.file_name().unwrap() != ".peerbrush-versions")
        .map(|p| {
            let bytes = fs::read(p.join("selected.appinstaller")).unwrap();
            (p, bytes)
        })
        .collect::<Vec<_>>();
    assert_eq!(retained.len(), 3);
    assert_eq!(interrupted.calls.lock().unwrap().len(), 6);
    let no_network = HttpFake {
        files: BTreeMap::new(),
        calls: Mutex::new(vec![]),
    };
    assert!(acquire(
        &temp.0,
        &policy(),
        &no_network,
        &NativeFake::new(),
        &control()
    )
    .unwrap_err()
    .contains("retirement is not implemented"));
    assert!(no_network.calls.lock().unwrap().is_empty());
    for (path, bytes) in retained {
        assert_eq!(bytes, xml());
        assert_eq!(fs::read(path.join("selected.appinstaller")).unwrap(), bytes);
        assert!(!path.join("prepared.json").exists());
    }
    let other = Temp::new();
    let lease = crate::file_safety::Lease::acquire(&other.0.join("acquisition.json")).unwrap();
    assert!(acquire(
        &other.0,
        &policy(),
        &no_network,
        &NativeFake::new(),
        &control()
    )
    .is_err());
    assert!(no_network.calls.lock().unwrap().is_empty());
    assert_eq!(fs::read_dir(&other.0).unwrap().count(), 1);
    drop(lease);
    fs::write(other.0.join("foreign"), b"keep").unwrap();
    assert!(acquire(
        &other.0,
        &policy(),
        &no_network,
        &NativeFake::new(),
        &control()
    )
    .is_err());
    assert_eq!(fs::read(other.0.join("foreign")).unwrap(), b"keep");
    assert!(no_network.calls.lock().unwrap().is_empty());
}

#[test]
fn held_identity_and_complete_content_detect_replacement_before_native_acceptance() {
    let temp = Temp::new();
    let path = temp.0.join("main.msix");
    fs::write(&path, b"main-native-bytes").unwrap();
    let c = control();
    let held = Held::open(&path, PACKAGE, &c).unwrap();
    #[cfg(unix)]
    {
        fs::rename(&path, temp.0.join("original.msix")).unwrap();
        fs::write(&path, b"main-native-bytes").unwrap();
        assert!(
            held.check(&c).is_err(),
            "Byte-equal replacement still has foreign identity"
        );
    }
    #[cfg(windows)]
    {
        assert!(
            fs::write(&path, b"evil-native-bytes").is_err(),
            "Native held input must deny writing"
        );
        assert!(
            fs::rename(&path, temp.0.join("foreign.msix")).is_err(),
            "Native held input must deny deleting/renaming"
        );
        assert!(held.check(&c).is_ok());
    }
}

#[test]
fn real_lease_contention_and_cancel_after_native_success_cannot_publish_or_replace_media() {
    let temp = Temp::new();
    let lease = crate::file_safety::Lease::acquire(&temp.0.join("prepared.json")).unwrap();
    let transport = HttpFake::new();
    assert!(prepare(
        &temp.0,
        &policy(),
        &transport,
        &NativeFake::new(),
        &control()
    )
    .is_err());
    assert!(transport.calls.lock().unwrap().is_empty());
    drop(lease);
    struct CancelVerified;
    impl Verifier for CancelVerified {
        fn source(&self, _: &Policy) -> Result<Source> {
            Ok(source())
        }
        fn media(
            &self,
            file: &Held,
            policy: &Policy,
            selection: &Selection,
            runtime: bool,
            c: &Control,
        ) -> Result<Option<Target>> {
            let target = NativeFake::new().media(file, policy, selection, runtime, c)?;
            if runtime {
                c.cancelled.store(true, Ordering::Release);
            }
            Ok(target)
        }
    }
    assert!(prepare(
        &temp.0,
        &policy(),
        &HttpFake::new(),
        &CancelVerified,
        &control()
    )
    .unwrap_err()
    .contains("canceled"));
    assert!(!temp.0.join("prepared.json").exists());
    assert_eq!(
        fs::read(temp.0.join("main.msix")).unwrap(),
        b"main-native-bytes"
    );
    assert_eq!(
        fs::read(temp.0.join("runtime.appx")).unwrap(),
        b"runtime-native-bytes"
    );
}

#[test]
fn native_pending_cancel_timeout_late_success_and_next_worker_remain_single_and_fail_closed() {
    let temp = Temp::new();
    let mut controller = Controller::load(&temp.0);
    assert_eq!(controller.root, temp.0.join("windows-prepared"));
    let release = controller.fake_prepare();
    controller.start().unwrap();
    assert!(controller.active.is_some());
    assert!(controller.start().is_err());
    controller.active.as_mut().unwrap().deadline = Instant::now() - Duration::from_secs(1);
    controller.tick();
    assert!(controller.view.busy && controller.view.message.contains("deadline"));
    assert!(controller
        .active
        .as_ref()
        .unwrap()
        .cancel
        .load(Ordering::Acquire));
    release.send(()).unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while controller.active.is_some() && Instant::now() < deadline {
        controller.tick();
        std::thread::yield_now();
    }
    assert!(
        !controller.view.busy && controller.view.version.is_none() && controller.active.is_none()
    );
    let release = controller.fake_prepare();
    controller.start().unwrap();
    release.send(()).unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while controller.active.is_some() && Instant::now() < deadline {
        controller.tick();
        std::thread::yield_now();
    }
    assert_eq!(controller.view.version, Some([1, 30, 1, 0]));
    controller.later();
    assert_eq!(controller.view.version, Some([1, 30, 1, 0]));
}

#[test]
fn every_default_entry_is_disabled_and_has_no_storage_or_native_side_effect() {
    let temp = Temp::new();
    let mut controller = Controller::load(&temp.0);
    assert!(!ENABLED && configured().is_none() && !controller.allowed());
    assert_eq!(controller.start().unwrap_err(), DISABLED);
    assert!(fs::read_dir(&temp.0).unwrap().next().is_none());
    #[cfg(windows)]
    {
        assert_eq!(native::session(|| Ok(())).unwrap_err(), DISABLED);
        assert_eq!(native::Verifier.source(&policy()).unwrap_err(), DISABLED);
    }
}

// A narrow native staging boundary: real files, native-source/media consumer,
// leases, durable records, controller and panel remain in the production path.
struct StageFake {
    release: Mutex<Option<mpsc::Receiver<()>>>,
    uncertain: bool,
    submissions: Arc<AtomicUsize>,
    cancellations: Arc<AtomicUsize>,
}
struct StageFakeOperation {
    release: mpsc::Receiver<()>,
    uncertain: bool,
    cancellations: Arc<AtomicUsize>,
}
impl Stager for StageFake {
    fn submit(
        &self,
        selected: &RetainedSelection,
        view: Arc<Mutex<View>>,
    ) -> Result<Box<dyn StageOperation>> {
        self.submissions.fetch_add(1, Ordering::AcqRel);
        assert_eq!(selected.prepared, receipt());
        assert_eq!(selected.package.media.sha256, hash(b"main-native-bytes"));
        assert_eq!(selected.runtime.media.sha256, hash(b"runtime-native-bytes"));
        let root = selected.owned.path.parent().unwrap();
        assert!(crate::file_safety::Lease::acquire(&root.join("acquisition.json")).is_err());
        assert!(crate::file_safety::Lease::acquire(&selected.journal.path).is_err());
        let intent: serde_json::Value = serde_json::from_slice(
            &fs::read(selected.owned.path.join("stage-intent.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(intent["prepared_sha256"], selected.journal.media.sha256);
        assert_eq!(
            intent["prepared"],
            serde_json::to_value(&selected.prepared).unwrap()
        );
        #[cfg(windows)]
        {
            assert!(OpenOptions::new()
                .write(true)
                .open(&selected.package.path)
                .is_err());
            assert!(OpenOptions::new()
                .write(true)
                .open(&selected.runtime.path)
                .is_err());
            assert!(fs::rename(
                &selected.owned.path,
                selected.owned.path.with_extension("moved")
            )
            .is_err());
        }
        {
            let mut view = view.lock().unwrap();
            view.phase = "Staging synthetic Windows media at the native boundary";
            view.percentage = Some(17);
        }
        Ok(Box::new(StageFakeOperation {
            release: self.release.lock().unwrap().take().unwrap(),
            uncertain: self.uncertain,
            cancellations: self.cancellations.clone(),
        }))
    }
}
impl StageOperation for StageFakeOperation {
    fn poll(&mut self) -> StageObservation {
        if self.uncertain {
            self.uncertain = false;
            return StageObservation::Uncertain;
        }
        match self.release.try_recv() {
            Ok(()) => StageObservation::Terminal(StageTerminal {
                hresult: Some(0),
                registered: Some(false),
                canceled: false,
                invalid_progress: false,
            }),
            Err(mpsc::TryRecvError::Empty) => StageObservation::Pending,
            Err(mpsc::TryRecvError::Disconnected) => StageObservation::Uncertain,
        }
    }
    fn cancel(&mut self) {
        assert_eq!(
            self.cancellations.fetch_add(1, Ordering::AcqRel),
            0,
            "The owned native operation receives cancellation only once"
        );
    }
}
pub(super) fn stage_fixture(
    root: &Path,
    control: &Control,
    release: mpsc::Receiver<()>,
    uncertain: bool,
) -> Result<Staged> {
    fs::create_dir_all(root).unwrap();
    acquire(
        root,
        &policy(),
        &HttpFake::new(),
        &NativeFake::new(),
        control,
    )?;
    stage(
        root,
        &policy(),
        &NativeFake::new(),
        &StageFake {
            release: Mutex::new(Some(release)),
            uncertain,
            submissions: Arc::new(AtomicUsize::new(0)),
            cancellations: Arc::new(AtomicUsize::new(0)),
        },
        control,
    )
}
fn wait_controller(controller: &mut Controller, ready: impl Fn(&Controller) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        controller.tick();
        if ready(controller) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "Actual staging worker did not reach its bounded fixture milestone"
        );
        std::thread::yield_now();
    }
}
fn stage_attempt(root: &Path) -> PathBuf {
    fs::read_dir(root)
        .unwrap()
        .map(|v| v.unwrap())
        .find(|v| {
            v.file_name()
                .to_str()
                .is_some_and(|n| n.starts_with("attempt-"))
        })
        .unwrap()
        .path()
}
#[test]
fn actual_staging_retains_real_media_leases_and_durable_outcomes_without_retry_or_registration() {
    for mode in ["success", "cancel", "timeout", "uncertain"] {
        let temp = Temp::new();
        let mut controller = Controller::load(&temp.0);
        let (release, _done) = controller.fake_staging(mode == "uncertain");
        controller.start_staging().unwrap();
        wait_controller(&mut controller, |v| v.view.percentage == Some(17));
        assert!(controller.view.busy && controller.view.staged_version.is_none());
        let root = temp.0.join("windows-prepared");
        let attempt = stage_attempt(&root);
        let before_main = fs::read(attempt.join("main.msix")).unwrap();
        let before_runtime = fs::read(attempt.join("runtime.appx")).unwrap();
        let before_prepared = fs::read(attempt.join("prepared.json")).unwrap();
        let before_intent = fs::read(attempt.join("stage-intent.json")).unwrap();
        assert!(!attempt.join("stage-outcome.json").exists());
        assert!(crate::file_safety::Lease::acquire(&root.join("acquisition.json")).is_err());
        assert!(controller.start_staging().is_err());
        if mode == "cancel" {
            controller.later();
        }
        if mode == "timeout" {
            controller.active.as_mut().unwrap().deadline = Instant::now();
            controller.tick();
        }
        release.send(()).unwrap();
        wait_controller(&mut controller, |v| !v.view.busy);
        assert_eq!(
            controller.view.staged_version,
            (mode == "success").then_some([1, 30, 1, 0])
        );
        assert!(controller.view.version.is_none());
        let record: serde_json::Value =
            serde_json::from_slice(&fs::read(attempt.join("stage-outcome.json")).unwrap()).unwrap();
        assert_eq!(record["terminal"]["registered"], false);
        assert_eq!(record["rejected"], mode != "success");
        assert_eq!(fs::read(attempt.join("main.msix")).unwrap(), before_main);
        assert_eq!(
            fs::read(attempt.join("runtime.appx")).unwrap(),
            before_runtime
        );
        assert_eq!(
            fs::read(attempt.join("prepared.json")).unwrap(),
            before_prepared
        );
        assert_eq!(
            fs::read(attempt.join("stage-intent.json")).unwrap(),
            before_intent
        );
        let no_network = HttpFake {
            files: BTreeMap::new(),
            calls: Mutex::new(vec![]),
        };
        assert!(acquire(
            &root,
            &policy(),
            &no_network,
            &NativeFake::new(),
            &control()
        )
        .is_err());
        assert!(no_network.calls.lock().unwrap().is_empty());
        assert!(stage(
            &root,
            &policy(),
            &NativeFake::new(),
            &StageFake {
                release: Mutex::new(None),
                uncertain: false,
                submissions: Arc::new(AtomicUsize::new(0)),
                cancellations: Arc::new(AtomicUsize::new(0))
            },
            &control()
        )
        .is_err());
    }
}
#[test]
fn actual_progress_interleaving_cannot_accept_a_terminal_observed_during_native_readback() {
    struct InterleavedStager {
        ready: mpsc::Sender<Arc<StageProgress>>,
        resume: Mutex<Option<mpsc::Receiver<()>>>,
    }
    struct InterleavedOperation {
        progress: Arc<StageProgress>,
        ready: Option<mpsc::Sender<Arc<StageProgress>>>,
        resume: mpsc::Receiver<()>,
    }
    impl Stager for InterleavedStager {
        fn submit(
            &self,
            selected: &RetainedSelection,
            view: Arc<Mutex<View>>,
        ) -> Result<Box<dyn StageOperation>> {
            assert_eq!(selected.prepared, receipt());
            assert!(selected.owned.path.join("stage-intent.json").is_file());
            assert!(crate::file_safety::Lease::acquire(&selected.journal.path).is_err());
            let progress = Arc::new(StageProgress::new(view));
            progress.percentage(17);
            Ok(Box::new(InterleavedOperation {
                progress,
                ready: Some(self.ready.clone()),
                resume: self.resume.lock().unwrap().take().unwrap(),
            }))
        }
    }
    impl StageOperation for InterleavedOperation {
        fn poll(&mut self) -> StageObservation {
            let progress = self.progress.clone();
            // The actual adapter uses this same method around Status/GetResults.
            // Pause after raw native fields exist but before terminal delivery.
            progress.poll(|| {
                let terminal = StageTerminal {
                    hresult: Some(0),
                    registered: Some(false),
                    canceled: false,
                    invalid_progress: false,
                };
                self.ready.take().unwrap().send(progress.clone()).unwrap();
                self.resume.recv_timeout(Duration::from_secs(5)).unwrap();
                StageObservation::Terminal(terminal)
            })
        }
        fn cancel(&mut self) {
            panic!(
                "The first native readback is terminal; no earlier pending/uncertain poll exists"
            );
        }
    }
    for fault in ["none", "percentage", "registration"] {
        let temp = Temp::new();
        let mut controller = Controller::load(&temp.0);
        let root = controller.root.clone();
        let worker_root = root.clone();
        let (ready, receive) = mpsc::channel();
        let (release, resume) = mpsc::channel();
        controller.next_stage = Some(Box::new(move |control| {
            fs::create_dir(&worker_root).unwrap();
            acquire(
                &worker_root,
                &policy(),
                &HttpFake::new(),
                &NativeFake::new(),
                control,
            )?;
            stage(
                &worker_root,
                &policy(),
                &NativeFake::new(),
                &InterleavedStager {
                    ready,
                    resume: Mutex::new(Some(resume)),
                },
                control,
            )
        }));
        controller.start_staging().unwrap();
        let progress = receive.recv_timeout(Duration::from_secs(5)).unwrap();
        let attempt = stage_attempt(&root);
        let before: Vec<_> = [
            "selected.appinstaller",
            "discovery.appinstaller",
            "main.msix",
            "runtime.appx",
            "prepared.json",
            "stage-intent.json",
        ]
        .into_iter()
        .map(|name| (name, fs::read(attempt.join(name)).unwrap()))
        .collect();
        assert!(crate::file_safety::Lease::acquire(&root.join("acquisition.json")).is_err());
        assert!(!attempt.join("stage-outcome.json").exists());
        match fault {
            "none" => progress.percentage(64),
            "percentage" => progress.percentage(101),
            "registration" => progress.invalidate(),
            _ => unreachable!(),
        }
        controller.tick();
        assert!(controller.view.busy && controller.view.staged_version.is_none());
        release.send(()).unwrap();
        wait_controller(&mut controller, |v| !v.view.busy);
        assert_eq!(
            controller.view.staged_version,
            (fault == "none").then_some([1, 30, 1, 0])
        );
        let record: serde_json::Value =
            serde_json::from_slice(&fs::read(attempt.join("stage-outcome.json")).unwrap()).unwrap();
        assert_eq!(record["terminal"]["hresult"], 0);
        assert_eq!(record["terminal"]["registered"], false);
        assert_eq!(record["terminal"]["canceled"], false);
        assert_eq!(record["terminal"]["invalid_progress"], fault != "none");
        assert_eq!(record["rejected"], fault != "none");
        let closed = progress.view.lock().unwrap().clone();
        progress.percentage(99);
        progress.percentage(101);
        progress.invalidate();
        let after = progress.view.lock().unwrap();
        assert_eq!(after.phase, closed.phase);
        assert_eq!(after.percentage, closed.percentage);
        drop(after);
        for (name, bytes) in before {
            assert_eq!(fs::read(attempt.join(name)).unwrap(), bytes);
        }
        assert!(crate::file_safety::Lease::acquire(&root.join("acquisition.json")).is_ok());
        assert!(reconcile(&attempt, &policy(), &NativeFake::new(), &control()).is_err());
        let no_network = HttpFake {
            files: BTreeMap::new(),
            calls: Mutex::new(vec![]),
        };
        assert!(acquire(
            &root,
            &policy(),
            &no_network,
            &NativeFake::new(),
            &control()
        )
        .is_err());
        assert!(no_network.calls.lock().unwrap().is_empty());
    }
}
#[test]
fn dropped_staging_controller_keeps_the_worker_and_unknown_evidence_until_real_terminal() {
    let temp = Temp::new();
    let mut controller = Controller::load(&temp.0);
    let (release, done) = controller.fake_staging(false);
    controller.start_staging().unwrap();
    wait_controller(&mut controller, |v| v.view.percentage == Some(17));
    let root = temp.0.join("windows-prepared");
    let attempt = stage_attempt(&root);
    drop(controller);
    assert!(crate::file_safety::Lease::acquire(&root.join("acquisition.json")).is_err());
    assert!(!attempt.join("stage-outcome.json").exists());
    release.send(()).unwrap();
    done.recv_timeout(Duration::from_secs(5)).unwrap();
    assert!(crate::file_safety::Lease::acquire(&root.join("acquisition.json")).is_ok());
    let record: serde_json::Value =
        serde_json::from_slice(&fs::read(attempt.join("stage-outcome.json")).unwrap()).unwrap();
    assert_eq!(record["rejected"], true);
    assert!(attempt.join("prepared.json").is_file() && attempt.join("main.msix").is_file());
}
#[test]
fn stage_only_default_gate_prevents_filesystem_or_native_submission() {
    let temp = Temp::new();
    let mut controller = Controller::load(&temp.0);
    assert!(!controller.allowed());
    assert!(controller.start_staging().is_err());
    assert!(!temp.0.join("windows-prepared").exists());
    assert!(configured().is_none() && !ENABLED);
}

#[test]
fn staging_revalidates_changed_bytes_source_and_signatures_before_native_submission() {
    for damage in ["main", "runtime", "journal", "source", "signature"] {
        let temp = Temp::new();
        acquire(
            &temp.0,
            &policy(),
            &HttpFake::new(),
            &NativeFake::new(),
            &control(),
        )
        .unwrap();
        let attempt = stage_attempt(&temp.0);
        match damage {
            "main" => fs::write(attempt.join("main.msix"), b"changed-code").unwrap(),
            "runtime" => fs::write(attempt.join("runtime.appx"), b"changed-runtime").unwrap(),
            "journal" => fs::write(attempt.join("prepared.json"), b"{}").unwrap(),
            _ => {}
        }
        let files: Vec<_> = ["main.msix", "runtime.appx", "prepared.json"]
            .into_iter()
            .map(|name| (name, fs::read(attempt.join(name)).unwrap()))
            .collect();
        let submissions = Arc::new(AtomicUsize::new(0));
        let mut verifier = NativeFake::new();
        verifier.changes = damage == "source";
        verifier.reject = damage == "signature";
        assert!(stage(
            &temp.0,
            &policy(),
            &verifier,
            &StageFake {
                release: Mutex::new(None),
                uncertain: false,
                submissions: submissions.clone(),
                cancellations: Arc::new(AtomicUsize::new(0)),
            },
            &control()
        )
        .is_err());
        assert_eq!(submissions.load(Ordering::Acquire), 0);
        assert!(!attempt.join("stage-intent.json").exists());
        for (name, bytes) in files {
            assert_eq!(fs::read(attempt.join(name)).unwrap(), bytes);
        }
    }
}
struct FaultStage(&'static str);
struct FaultOperation(&'static str);
impl Stager for FaultStage {
    fn submit(
        &self,
        selection: &RetainedSelection,
        _: Arc<Mutex<View>>,
    ) -> Result<Box<dyn StageOperation>> {
        assert!(selection.owned.path.join("stage-intent.json").is_file());
        if self.0 == "submit-error" {
            return Err("Synthetic provider submission is unknown".into());
        }
        if self.0 == "outcome-damage" {
            fs::write(
                selection.owned.path.join("stage-outcome.json"),
                b"keep foreign bytes",
            )
            .unwrap();
        }
        Ok(Box::new(FaultOperation(self.0)))
    }
}
impl StageOperation for FaultOperation {
    fn poll(&mut self) -> StageObservation {
        StageObservation::Terminal(StageTerminal {
            hresult: if self.0 == "missing-code" {
                None
            } else if self.0 == "provider-error" {
                Some(-1)
            } else {
                Some(0)
            },
            registered: Some(self.0 == "registered"),
            canceled: false,
            invalid_progress: false,
        })
    }
    fn cancel(&mut self) {
        panic!("Terminal synthetic operation needs no cancellation");
    }
}
#[test]
fn staging_failed_unknown_registered_or_damaged_outcomes_keep_intent_and_block_retries() {
    for fault in [
        "submit-error",
        "provider-error",
        "missing-code",
        "registered",
        "outcome-damage",
    ] {
        let temp = Temp::new();
        acquire(
            &temp.0,
            &policy(),
            &HttpFake::new(),
            &NativeFake::new(),
            &control(),
        )
        .unwrap();
        let attempt = stage_attempt(&temp.0);
        let media: Vec<_> = ["main.msix", "runtime.appx", "prepared.json"]
            .into_iter()
            .map(|name| (name, fs::read(attempt.join(name)).unwrap()))
            .collect();
        let result = stage(
            &temp.0,
            &policy(),
            &NativeFake::new(),
            &FaultStage(fault),
            &control(),
        );
        assert!(
            result.as_ref().is_err() || !result.unwrap().accepted,
            "{fault}"
        );
        assert!(attempt.join("stage-intent.json").is_file());
        assert!(reconcile(&attempt, &policy(), &NativeFake::new(), &control()).is_err());
        assert!(acquire(
            &temp.0,
            &policy(),
            &HttpFake::new(),
            &NativeFake::new(),
            &control()
        )
        .is_err());
        if fault == "outcome-damage" {
            assert_eq!(
                fs::read(attempt.join("stage-outcome.json")).unwrap(),
                b"keep foreign bytes"
            );
        }
        for (name, bytes) in media {
            assert_eq!(fs::read(attempt.join(name)).unwrap(), bytes);
        }
    }
}

#[test]
fn production_staging_root_requires_real_default_profile_and_worker_retains_its_session_lock() {
    let temp = Temp::new();
    let locations = crate::settings::Locations {
        config: temp.0.join("config"),
        data: temp.0.join("data"),
        cache: temp.0.join("cache"),
    };
    let legacy = temp.0.join("legacy");
    fs::create_dir(&legacy).unwrap();
    let profile = crate::settings::Profile::open(locations.clone(), &legacy).unwrap();
    let mut foreign = Controller::load(&temp.0.join("foreign"));
    assert!(foreign.staging_profile().is_err());
    foreign.set_profile(Some(profile.update_profile()));
    assert!(foreign.staging_profile().is_err());
    assert!(!temp.0.join("foreign").exists());
    drop(foreign);
    let mut controller = Controller::load(&locations.profile());
    controller.set_profile(Some(profile.update_profile()));
    assert!(controller.staging_profile().is_ok());
    let (release, done) = controller.fake_staging(false);
    controller.start_staging().unwrap();
    wait_controller(&mut controller, |v| v.view.percentage == Some(17));
    drop(profile);
    drop(controller);
    assert!(
        crate::settings::Profile::open(locations.clone(), &legacy).is_err(),
        "An owned native operation must retain the actual profile lease after the UI owner drops"
    );
    release.send(()).unwrap();
    done.recv_timeout(Duration::from_secs(5)).unwrap();
    let reopened = crate::settings::Profile::open(locations, &legacy).unwrap();
    assert!(reopened
        .locations
        .profile()
        .join("windows-prepared")
        .exists());
    drop(reopened);
}

// Only native signature/source polling is controlled. The records/media reader,
// default-profile binding, retained handles/leases and controller are actual.
pub(super) fn inspect_fixture(
    root: &Path,
    control: &Control,
    resume: mpsc::Receiver<()>,
    entered: mpsc::Sender<()>,
) -> Result<Reconciled> {
    struct NativeGate {
        native: NativeFake,
        resume: Mutex<Option<mpsc::Receiver<()>>>,
        entered: mpsc::Sender<()>,
    }
    impl Verifier for NativeGate {
        fn source(&self, policy: &Policy) -> Result<Source> {
            self.native.source(policy)
        }
        fn media(
            &self,
            file: &Held,
            policy: &Policy,
            selection: &Selection,
            runtime: bool,
            control: &Control,
        ) -> Result<Option<Target>> {
            if let Some(resume) = self.resume.lock().unwrap().take() {
                self.entered.send(()).unwrap();
                resume
                    .recv_timeout(Duration::from_secs(5))
                    .map_err(|_| "Synthetic native readback was not released.")?;
            }
            self.native.media(file, policy, selection, runtime, control)
        }
    }
    inspect_retained(
        root,
        &policy(),
        &NativeGate {
            native: NativeFake::new(),
            resume: Mutex::new(Some(resume)),
            entered,
        },
        control,
    )
}
fn completed_stage(root: &Path) -> PathBuf {
    let (release, resume) = mpsc::channel();
    release.send(()).unwrap();
    let result = stage_fixture(root, &control(), resume, false).unwrap();
    assert!(result.accepted);
    stage_attempt(root)
}
#[test]
fn retained_inspection_reopens_actual_stage_bytes_without_claiming_installation_or_retiring_them() {
    let temp = Temp::new();
    let attempt = completed_stage(&temp.0);
    let before: Vec<_> = [
        "selected.appinstaller",
        "discovery.appinstaller",
        "main.msix",
        "runtime.appx",
        "prepared.json",
        "stage-intent.json",
        "stage-outcome.json",
    ]
    .into_iter()
    .map(|name| (name, fs::read(attempt.join(name)).unwrap()))
    .collect();
    let c = control();
    assert!(matches!(
        inspect_retained(&temp.0, &policy(), &NativeFake::new(), &c).unwrap(),
        Reconciled::RecordedStaging([1, 30, 1, 0])
    ));
    assert_eq!(
        c.view.lock().unwrap().installed_version,
        Some([1, 30, 0, 0])
    );
    assert!(reconcile(&attempt, &policy(), &NativeFake::new(), &control()).is_err());
    let network = HttpFake {
        files: BTreeMap::new(),
        calls: Mutex::new(vec![]),
    };
    assert!(acquire(&temp.0, &policy(), &network, &NativeFake::new(), &control()).is_err());
    assert!(network.calls.lock().unwrap().is_empty());
    let before_outcome = fs::read(attempt.join("stage-outcome.json")).unwrap();
    for (name, bytes) in before {
        assert_eq!(fs::read(attempt.join(name)).unwrap(), bytes);
    }
    let missing = temp.0.join("never-created");
    assert!(matches!(
        inspect_retained(&missing, &policy(), &NativeFake::new(), &control()).unwrap(),
        Reconciled::Empty
    ));
    assert!(!missing.exists());
    let current = control();
    assert!(inspect_retained(&temp.0, &policy(), &SelectedSource, &current).is_err());
    assert_eq!(
        current.view.lock().unwrap().installed_version,
        Some([1, 30, 1, 0])
    );
    assert_eq!(
        fs::read(attempt.join("stage-outcome.json")).unwrap(),
        before_outcome
    );
}
struct SelectedSource;
impl Verifier for SelectedSource {
    fn source(&self, _: &Policy) -> Result<Source> {
        let mut current = source();
        current.version = [1, 30, 1, 0];
        current.full_name = "PeerBrush.Synthetic_1.30.1.0_x64__abc123".into();
        current.build = "2".repeat(40);
        current.association = new_descriptor();
        Ok(current)
    }
    fn media(
        &self,
        file: &Held,
        policy: &Policy,
        selected: &Selection,
        runtime: bool,
        control: &Control,
    ) -> Result<Option<Target>> {
        NativeFake::new().media(file, policy, selected, runtime, control)
    }
}
struct RunningNative {
    root: PathBuf,
    executing: PathBuf,
    current: Source,
    proof: bool,
    source_calls: AtomicUsize,
    change_after: usize,
    current_media_calls: AtomicUsize,
    new_main_calls: AtomicUsize,
    archive_fault: &'static str,
    target: Target,
}
impl RunningNative {
    fn new(root: &Path) -> Self {
        fs::create_dir(root).unwrap();
        fs::write(root.join("peerbrush.exe"), b"selected-native-executable").unwrap();
        fs::write(
            root.join("PeerBrush-0.30.1-source.zip"),
            b"selected-native-source",
        )
        .unwrap();
        let target = selected_target();
        let mut current = source();
        current.full_name = target.full_name;
        current.version = target.version;
        current.build = target.build;
        current.association = new_descriptor();
        Self {
            root: root.into(),
            executing: root.join("peerbrush.exe"),
            current,
            proof: true,
            source_calls: AtomicUsize::new(0),
            change_after: usize::MAX,
            current_media_calls: AtomicUsize::new(0),
            new_main_calls: AtomicUsize::new(0),
            archive_fault: "none",
            target: selected_target(),
        }
    }
}
impl Verifier for RunningNative {
    fn source(&self, _: &Policy) -> Result<Source> {
        let mut current = self.current.clone();
        if self.source_calls.fetch_add(1, Ordering::Relaxed) >= self.change_after {
            current.full_name.push_str("-newer");
        }
        Ok(current)
    }
    fn media(
        &self,
        file: &Held,
        policy: &Policy,
        selection: &Selection,
        runtime: bool,
        control: &Control,
    ) -> Result<Option<Target>> {
        if !runtime {
            self.new_main_calls.fetch_add(1, Ordering::Relaxed);
        }
        let verified = NativeFake::new().media(file, policy, selection, runtime, control)?;
        Ok(if self.proof { verified } else { None })
    }
    fn current_media(
        &self,
        file: &Held,
        policy: &Policy,
        current: &Source,
        control: &Control,
    ) -> Result<Target> {
        self.current_media_calls.fetch_add(1, Ordering::Relaxed);
        file.check(control)?;
        if !self.proof
            || self.archive_fault == "signature"
            || file.media.sha256 != hash(b"main-native-bytes")
        {
            return Err("Synthetic current signature or signed payload rejected".into());
        }
        if &self.source(policy)? != current {
            return Err(STAGE_HOLD.into());
        }
        let mut target = self.target.clone();
        match self.archive_fault {
            "manifest-version" => target.version[2] += 1,
            "signed-build" => target.build = "9".repeat(40),
            "full-name" => target.full_name.push_str("-foreign"),
            "signed-executable" => target.executable.sha256 = "9".repeat(64),
            "signed-source" => target.source.sha256 = "9".repeat(64),
            _ => {}
        }
        if !target.matches(current, policy) {
            return Err(STAGE_HOLD.into());
        }
        file.check(control)?;
        Ok(target)
    }
    fn running_target(
        &self,
        _: &Policy,
        target: &Target,
        control: &Control,
    ) -> Result<RunningTarget> {
        RunningTarget::open(
            &self.root,
            &self.executing,
            self.source(&policy())?,
            target,
            control,
        )
    }
}
#[test]
fn selected_running_target_requires_verified_payload_bytes_and_never_changes_old_source_admission()
{
    for recorded in [false, true] {
        let temp = Temp::new();
        let cache = temp.0.join("windows-prepared");
        let attempt = if recorded {
            completed_stage(&cache)
        } else {
            fs::create_dir(&cache).unwrap();
            acquire(
                &cache,
                &policy(),
                &HttpFake::new(),
                &NativeFake::new(),
                &control(),
            )
            .unwrap();
            stage_attempt(&cache)
        };
        let native = RunningNative::new(&temp.0.join("installed-target"));
        let names = [
            "selected.appinstaller",
            "discovery.appinstaller",
            "main.msix",
            "runtime.appx",
            "prepared.json",
            "stage-intent.json",
            "stage-outcome.json",
        ];
        let original: Vec<_> = names
            .into_iter()
            .filter_map(|name| fs::read(attempt.join(name)).ok().map(|bytes| (name, bytes)))
            .collect();
        let observation = control();
        assert!(matches!(
            inspect_retained(&cache, &policy(), &native, &observation).unwrap(),
            Reconciled::SelectedTargetRunning([1, 30, 1, 0])
        ));
        assert_eq!(
            observation.view.lock().unwrap().installed_version,
            Some([1, 30, 1, 0])
        );
        // Identical selected bytes do not turn NEW identity into the old download/stage source.
        assert!(reconcile(&attempt, &policy(), &native, &control()).is_err());
        let network = HttpFake {
            files: BTreeMap::new(),
            calls: Mutex::new(vec![]),
        };
        assert!(acquire(&cache, &policy(), &network, &native, &control()).is_err());
        assert!(network.calls.lock().unwrap().is_empty());
        assert!(stage(
            &cache,
            &policy(),
            &native,
            &FaultStage("submit-error"),
            &control()
        )
        .is_err());
        for (name, bytes) in original {
            assert_eq!(fs::read(attempt.join(name)).unwrap(), bytes);
        }
        if !recorded {
            assert!(!attempt.join("stage-intent.json").exists());
        }
    }
}
#[test]
fn selected_target_json_native_identity_file_mutation_and_late_source_changes_cannot_claim_running()
{
    for fault in [
        "no-proof",
        "full-name",
        "version",
        "build",
        "family",
        "association",
        "executable",
        "source",
        "wrong-process-path",
        "late-source",
        "json-claim",
    ] {
        let temp = Temp::new();
        let cache = temp.0.join("windows-prepared");
        let attempt = completed_stage(&cache);
        let mut native = RunningNative::new(&temp.0.join("installed-target"));
        match fault {
            "no-proof" => native.proof = false,
            "full-name" => native.current.full_name.push_str("-foreign"),
            "version" => native.current.version[2] += 1,
            "build" => native.current.build = "3".repeat(40),
            "family" => native.current.family.push_str("-foreign"),
            "association" => native.current.association.push_str("-foreign"),
            "executable" => fs::write(
                native.root.join("peerbrush.exe"),
                b"changed executable bytes",
            )
            .unwrap(),
            "source" => fs::write(
                native.root.join("PeerBrush-0.30.1-source.zip"),
                b"changed source bytes",
            )
            .unwrap(),
            "wrong-process-path" => {
                native.executing = native.root.join("foreign.exe");
                fs::write(&native.executing, b"selected-native-executable").unwrap();
            }
            "late-source" => native.change_after = 3,
            "json-claim" => {
                let path = attempt.join("prepared.json");
                let mut claim: serde_json::Value =
                    serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
                claim["target"] =
                    serde_json::json!({"version":[1,30,1,0],"verified":true,"healthy":true});
                fs::write(path, serde_json::to_vec(&claim).unwrap()).unwrap();
            }
            _ => unreachable!(),
        }
        let original: Vec<_> = fs::read_dir(&attempt)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .filter(|path| path.is_file())
            .map(|path| {
                let bytes = fs::read(&path).unwrap();
                (path, bytes)
            })
            .collect();
        assert!(
            inspect_retained(&cache, &policy(), &native, &control()).is_err(),
            "{fault}"
        );
        for (path, bytes) in original {
            assert_eq!(fs::read(path).unwrap(), bytes, "{fault}");
        }
        assert!(attempt.join("stage-intent.json").exists());
        assert!(attempt.join("stage-outcome.json").exists());
    }
}
#[test]
fn actual_selected_target_startup_worker_retains_profile_and_late_cancel_cannot_promote_observation(
) {
    struct Release(Option<mpsc::Sender<()>>, mpsc::Receiver<()>);
    impl Drop for Release {
        fn drop(&mut self) {
            if let Some(sender) = self.0.take() {
                let _ = sender.send(());
            }
            let _ = self.1.recv_timeout(Duration::from_secs(5));
        }
    }
    for cancel in [false, true] {
        let temp = Temp::new();
        let locations = crate::settings::Locations {
            config: temp.0.join("config"),
            data: temp.0.join("data"),
            cache: temp.0.join("cache"),
        };
        let legacy = temp.0.join("legacy");
        fs::create_dir(&legacy).unwrap();
        let profile = crate::settings::Profile::open(locations.clone(), &legacy).unwrap();
        let cache = locations.profile().join("windows-prepared");
        let attempt = completed_stage(&cache);
        let outcome = fs::read(attempt.join("stage-outcome.json")).unwrap();
        let native = RunningNative::new(&temp.0.join("installed-target"));
        let lease = profile.update_profile();
        let (ready, entered) = mpsc::channel();
        let (sender, resume) = mpsc::channel();
        let (finished, done) = mpsc::channel();
        let mut release = Release(Some(sender), done);
        let mut controller = Controller::load(&locations.profile());
        controller.set_profile(Some(profile.update_profile()));
        controller.next_reconcile = Some(Box::new(move |control| {
            let _profile = lease;
            _profile.validate()?;
            let result = inspect_retained(&cache, &policy(), &native, control);
            ready
                .send(())
                .map_err(|_| "Synthetic observer lost its owner")?;
            resume
                .recv_timeout(Duration::from_secs(5))
                .map_err(|_| "Synthetic observer was not released")?;
            drop(_profile);
            let _ = finished.send(());
            result
        }));
        controller.startup();
        entered.recv_timeout(Duration::from_secs(5)).unwrap();
        controller.set_profile(None);
        drop(profile);
        assert!(crate::settings::Profile::open(locations.clone(), &legacy).is_err());
        if cancel {
            controller.later();
        }
        release.0.take().unwrap().send(()).unwrap();
        wait_controller(&mut controller, |value| !value.view.busy);
        assert_eq!(
            controller.view.observed_target,
            (!cancel).then_some([1, 30, 1, 0])
        );
        assert!(controller.view.version.is_none() && controller.view.staged_version.is_none());
        assert!(!controller.view.recorded_staging && controller.deployment_blocker().is_some());
        assert!(controller.start().is_err() && controller.start_staging().is_err());
        assert_eq!(
            fs::read(attempt.join("stage-outcome.json")).unwrap(),
            outcome
        );
        if !cancel {
            let context = eframe::egui::Context::default();
            let rendered = context.run(Default::default(), |context| {
                eframe::egui::CentralPanel::default().show(context, |ui| controller.show(ui));
            });
            assert!(rendered.shapes.iter().any(|shape| matches!(&shape.shape,
                eframe::egui::Shape::Text(text) if text.galley.job.text == "Observed selected target 1.30.1.0 · health unverified")));
        }
        drop(controller);
        drop(crate::settings::Profile::open(locations, &legacy).unwrap());
    }
}
#[test]
fn damaged_missing_rejected_or_foreign_stage_records_never_become_reusable_or_installed() {
    for damage in [
        "intent",
        "missing-outcome",
        "outcome-binding",
        "rejected",
        "registered",
        "progress",
        "canceled",
        "code",
        "media",
        "source",
    ] {
        let temp = Temp::new();
        let attempt = completed_stage(&temp.0);
        match damage {
            "intent" => {
                fs::write(attempt.join("stage-intent.json"), b"{").unwrap();
            }
            "missing-outcome" => {
                fs::remove_file(attempt.join("stage-outcome.json")).unwrap();
            }
            "media" => {
                fs::write(attempt.join("runtime.appx"), b"foreign bytes").unwrap();
            }
            "source" => {}
            _ => {
                let path = attempt.join("stage-outcome.json");
                let mut value: serde_json::Value =
                    serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
                match damage {
                    "outcome-binding" => value["intent_sha256"] = serde_json::json!("f".repeat(64)),
                    "rejected" => value["rejected"] = true.into(),
                    "registered" => value["terminal"]["registered"] = true.into(),
                    "progress" => value["terminal"]["invalid_progress"] = true.into(),
                    "canceled" => value["terminal"]["canceled"] = true.into(),
                    "code" => value["terminal"]["hresult"] = serde_json::Value::Null,
                    _ => unreachable!(),
                }
                fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();
            }
        }
        let before: Vec<_> = fs::read_dir(&attempt)
            .unwrap()
            .map(|v| v.unwrap().path())
            .filter(|p| p.is_file())
            .map(|p| {
                let b = fs::read(&p).unwrap();
                (p, b)
            })
            .collect();
        let mut verifier = NativeFake::new();
        verifier.changes = damage == "source";
        assert!(
            inspect_retained(&temp.0, &policy(), &verifier, &control()).is_err(),
            "{damage}"
        );
        assert!(reconcile(&attempt, &policy(), &NativeFake::new(), &control()).is_err());
        for (path, bytes) in before {
            assert_eq!(fs::read(path).unwrap(), bytes);
        }
        assert!(attempt.join("stage-intent.json").exists());
    }
}
#[test]
fn startup_reconciliation_retains_default_profile_and_cancel_cannot_promote_late_recorded_success()
{
    for mode in ["complete", "cancel-native", "cancel-after-readback"] {
        let cancel = mode != "complete";
        let temp = Temp::new();
        let locations = crate::settings::Locations {
            config: temp.0.join("config"),
            data: temp.0.join("data"),
            cache: temp.0.join("cache"),
        };
        let legacy = temp.0.join("legacy");
        fs::create_dir(&legacy).unwrap();
        let profile = crate::settings::Profile::open(locations.clone(), &legacy).unwrap();
        let root = locations.profile().join("windows-prepared");
        let attempt = completed_stage(&root);
        let original = fs::read(attempt.join("stage-outcome.json")).unwrap();
        let mut controller = Controller::load(&locations.profile());
        controller.set_profile(Some(profile.update_profile()));
        let (release, ready, done) = controller.fake_reconciliation();
        controller.startup();
        ready.recv_timeout(Duration::from_secs(5)).unwrap();
        controller.set_profile(None);
        drop(profile);
        assert!(
            crate::settings::Profile::open(locations.clone(), &legacy).is_err(),
            "The real reader worker retains its captured default-profile lease"
        );
        assert!(controller.deployment_blocker().is_some());
        assert!(controller.start().is_err() && controller.start_staging().is_err());
        assert!(crate::file_safety::Lease::acquire(&root.join("acquisition.json")).is_err());
        if mode == "cancel-native" {
            controller.later();
        }
        release.send(()).unwrap();
        done.recv_timeout(Duration::from_secs(5)).unwrap();
        // The complete real reader has returned before this cancellation, but
        // the actual controller has not consumed its successful result yet.
        if mode == "cancel-after-readback" {
            controller.later();
        }
        wait_controller(&mut controller, |v| !v.view.busy);
        assert_eq!(controller.view.recorded_staging, !cancel);
        assert_eq!(
            controller.view.staged_version,
            (!cancel).then_some([1, 30, 1, 0])
        );
        assert!(controller.deployment_blocker().is_some());
        assert!(controller.start().is_err() && controller.start_staging().is_err());
        assert_eq!(
            fs::read(attempt.join("stage-outcome.json")).unwrap(),
            original
        );
        drop(controller);
        drop(crate::settings::Profile::open(locations, &legacy).unwrap());
    }
    let temp = Temp::new();
    let mut disabled = Controller::load(&temp.0);
    disabled.startup();
    assert!(disabled.start_reconciliation().is_err());
    assert!(disabled.deployment_blocker().is_none() && !disabled.view.busy);
    assert!(!temp.0.join("windows-prepared").exists());
}

#[test]
fn current_signed_archive_uses_the_actual_selected_consumer_without_new_target_admission() {
    for recorded in [false, true] {
        let temp = Temp::new();
        let root = temp.0.join("windows-prepared");
        let attempt = if recorded {
            completed_stage(&root)
        } else {
            fs::create_dir(&root).unwrap();
            acquire(
                &root,
                &policy(),
                &HttpFake::new(),
                &NativeFake::new(),
                &control(),
            )
            .unwrap();
            stage_attempt(&root)
        };
        let native = RunningNative::new(&temp.0.join("current-installed"));
        let archive = fs::read(attempt.join("main.msix")).unwrap();
        assert!(matches!(
            inspect_retained(&root, &policy(), &native, &control()).unwrap(),
            Reconciled::SelectedTargetRunning([1, 30, 1, 0])
        ));
        assert_eq!(native.current_media_calls.load(Ordering::Relaxed), 1);
        assert_eq!(
            native.new_main_calls.load(Ordering::Relaxed),
            0,
            "The current archive must not be verified twice or admitted by the newer-target path"
        );
        assert_eq!(fs::read(attempt.join("main.msix")).unwrap(), archive);
        assert!(descriptor(&xml(), &policy(), &native.current).is_err());
        assert!(reconcile(&attempt, &policy(), &native, &control()).is_err());
        let network = HttpFake {
            files: BTreeMap::new(),
            calls: Mutex::new(vec![]),
        };
        assert!(acquire(&root, &policy(), &network, &native, &control()).is_err());
        assert!(network.calls.lock().unwrap().is_empty());
        assert!(stage(
            &root,
            &policy(),
            &native,
            &FaultStage("submit-error"),
            &control()
        )
        .is_err());
    }
}

#[test]
fn current_archive_signature_manifest_identity_and_signed_payload_mismatches_remain_unknown() {
    for fault in [
        "signature",
        "manifest-version",
        "signed-build",
        "full-name",
        "signed-executable",
        "signed-source",
    ] {
        let temp = Temp::new();
        let root = temp.0.join("windows-prepared");
        let attempt = completed_stage(&root);
        let before: Vec<_> = [
            "prepared.json",
            "main.msix",
            "runtime.appx",
            "selected.appinstaller",
            "discovery.appinstaller",
            "stage-intent.json",
            "stage-outcome.json",
        ]
        .into_iter()
        .map(|name| (name, fs::read(attempt.join(name)).unwrap()))
        .collect();
        let mut native = RunningNative::new(&temp.0.join("current-installed"));
        native.archive_fault = fault;
        assert!(
            inspect_retained(&root, &policy(), &native, &control()).is_err(),
            "{fault}"
        );
        assert_eq!(native.current_media_calls.load(Ordering::Relaxed), 1);
        assert_eq!(native.new_main_calls.load(Ordering::Relaxed), 0);
        for (name, bytes) in before {
            assert_eq!(
                fs::read(attempt.join(name)).unwrap(),
                bytes,
                "{fault}: {name}"
            );
        }
    }
}

#[test]
fn current_archive_proof_keeps_real_held_media_and_rejects_restamped_or_replaced_bytes() {
    for replacement in [false, true] {
        let temp = Temp::new();
        let root = temp.0.join("windows-prepared");
        let attempt = completed_stage(&root);
        let native = RunningNative::new(&temp.0.join("current-installed"));
        let c = control();
        let current = native.source(&policy()).unwrap();
        let retained = authenticate_media(
            read_prepared(&attempt, &policy(), &c).unwrap(),
            &policy(),
            &native,
            Some(&current),
            &c,
        )
        .unwrap();
        let proof = current_archive(&retained, &current, &policy(), &native, &c).unwrap();
        let path = attempt.join("main.msix");
        let original = fs::read(&path).unwrap();
        let modified = fs::metadata(&path).unwrap().modified().unwrap();
        #[cfg(unix)]
        {
            if replacement {
                fs::rename(&path, attempt.join("held-current-original.msix")).unwrap();
                fs::write(&path, &original).unwrap();
            } else {
                let mut altered = original.clone();
                altered[0] ^= 1;
                fs::write(&path, &altered).unwrap();
            }
            fs::OpenOptions::new()
                .write(true)
                .open(&path)
                .unwrap()
                .set_times(fs::FileTimes::new().set_modified(modified))
                .unwrap();
            assert_eq!(fs::metadata(&path).unwrap().len(), original.len() as u64);
            assert_eq!(fs::metadata(&path).unwrap().modified().unwrap(), modified);
            assert!(proof.check(&policy(), &native, &c).is_err());
        }
        #[cfg(windows)]
        {
            if replacement {
                assert!(fs::rename(&path, attempt.join("held-current-original.msix")).is_err());
            } else {
                let mut altered = original.clone();
                altered[0] ^= 1;
                assert!(fs::write(&path, &altered).is_err());
                assert!(fs::OpenOptions::new().write(true).open(&path).is_err());
            }
            assert_eq!(fs::read(&path).unwrap(), original);
            assert_eq!(fs::metadata(&path).unwrap().modified().unwrap(), modified);
            proof.check(&policy(), &native, &c).unwrap();
        }
        assert!(
            attempt.join("stage-intent.json").exists()
                && attempt.join("stage-outcome.json").exists()
        );
    }
}

#[test]
fn current_archive_reopen_rejects_same_size_restamped_content_before_native_acceptance() {
    let temp = Temp::new();
    let root = temp.0.join("windows-prepared");
    let attempt = completed_stage(&root);
    let native = RunningNative::new(&temp.0.join("current-installed"));
    let path = attempt.join("main.msix");
    let original = fs::read(&path).unwrap();
    let modified = fs::metadata(&path).unwrap().modified().unwrap();
    let mut altered = original.clone();
    altered[0] ^= 1;
    fs::write(&path, &altered).unwrap();
    fs::OpenOptions::new()
        .write(true)
        .open(&path)
        .unwrap()
        .set_times(fs::FileTimes::new().set_modified(modified))
        .unwrap();
    assert_eq!(fs::metadata(&path).unwrap().len(), original.len() as u64);
    assert_eq!(fs::metadata(&path).unwrap().modified().unwrap(), modified);
    assert!(inspect_retained(&root, &policy(), &native, &control()).is_err());
    assert_eq!(
        native.current_media_calls.load(Ordering::Relaxed),
        0,
        "Retained bytes must match the actual Prepared digest before native trust"
    );
    assert_eq!(native.new_main_calls.load(Ordering::Relaxed), 0);
    assert_eq!(fs::read(&path).unwrap(), altered);
    assert!(
        attempt.join("stage-intent.json").exists() && attempt.join("stage-outcome.json").exists()
    );
}

fn initial_current_native(root: &Path) -> RunningNative {
    let mut native = RunningNative::new(root);
    native.current = source();
    native.target.full_name = native.current.full_name.clone();
    native.target.version = native.current.version;
    native.target.build = native.current.build.clone();
    native.target.source_name = "PeerBrush-0.30.0-source.zip".into();
    fs::rename(
        root.join("PeerBrush-0.30.1-source.zip"),
        root.join(&native.target.source_name),
    )
    .unwrap();
    native
}
fn current_http() -> HttpFake {
    let mut http = HttpFake::new();
    let bytes = String::from_utf8(xml())
        .unwrap()
        .replace("1.30.1.0", "1.30.0.0")
        .into_bytes();
    http.files
        .insert(source().association, (bytes.clone(), bytes.len() as u64));
    http.files.insert(
        "https://example.invalid/windows/PeerBrush-1.30.0.0.msix".into(),
        (b"main-native-bytes".to_vec(), 17),
    );
    http
}

#[test]
fn stable_new_and_immutable_current_routes_work_without_an_opportunistic_prior_capture() {
    let temp = Temp::new();
    let root = temp.0.join("windows-prepared");
    fs::create_dir(&root).unwrap();
    let native = initial_current_native(&temp.0.join("installed"));
    // The stable route already offers NEW; the original association still
    // serves CURRENT. No versioned URL is synthesized by the consumer.
    let http = current_http();
    let prepared = acquire_with_current(&root, &policy(), &http, &native, &control()).unwrap();
    assert_eq!(prepared, receipt());
    assert_eq!(
        *http.calls.lock().unwrap(),
        vec![
            source().association,
            "https://example.invalid/windows/PeerBrush-1.30.0.0.msix".into(),
            policy().runtime_uri,
            policy().discovery,
            new_descriptor(),
            "https://example.invalid/windows/PeerBrush-1.30.1.0.msix".into(),
            policy().runtime_uri,
        ]
    );
    let current = captured_current(&root);
    let new = attempts(&root).unwrap().1.unwrap();
    assert_eq!(
        fs::read(current.join("main.msix")).unwrap(),
        b"main-native-bytes"
    );
    assert_eq!(fs::read(new.join("discovery.appinstaller")).unwrap(), xml());
    assert_eq!(fs::read(new.join("selected.appinstaller")).unwrap(), xml());
    assert!(current.join("current.json").exists());
    assert!(!current.join("stage-intent.json").exists());
    assert!(!new.join("stage-intent.json").exists());
    assert!(!ENABLED && configured().is_none());
}

#[test]
fn stable_discovery_requires_exact_immutable_readback_before_any_native_media_or_package_io() {
    for bad in [
        "different-bytes",
        "missing",
        "foreign-directory",
        "stable-root",
        "downgrade",
    ] {
        let temp = Temp::new();
        let mut http = HttpFake::new();
        match bad {
            "different-bytes" => {
                let entry = http.files.get_mut(&new_descriptor()).unwrap();
                entry.0.push(b'\n');
                entry.1 += 1;
            }
            "missing" => {
                http.files.remove(&new_descriptor());
            }
            _ => {
                let text = String::from_utf8(xml()).unwrap();
                let bytes = match bad {
                    "foreign-directory" => text.replace("/windows/", "/unapproved/"),
                    "stable-root" => text.replace(&new_descriptor(), &policy().discovery),
                    _ => text.replace("1.30.1.0", "1.30.0.0"),
                }
                .into_bytes();
                http.files
                    .insert(policy().discovery, (bytes.clone(), bytes.len() as u64));
            }
        }
        let native = NativeFake::new();
        assert!(
            prepare(&temp.0, &policy(), &http, &native, &control()).is_err(),
            "{bad}"
        );
        assert_eq!(native.media_calls.load(Ordering::Relaxed), 0, "{bad}");
        assert!(!temp.0.join("main.msix").exists());
        assert!(!temp.0.join("prepared.json").exists());
        assert_eq!(http.calls.lock().unwrap()[0], policy().discovery);
    }
}

#[test]
fn reopened_new_selection_rechecks_the_retained_stable_bytes_without_network_or_json_authority() {
    let temp = Temp::new();
    prepare(
        &temp.0,
        &policy(),
        &HttpFake::new(),
        &NativeFake::new(),
        &control(),
    )
    .unwrap();
    let marker = fs::read(temp.0.join("prepared.json")).unwrap();
    let path = temp.0.join("discovery.appinstaller");
    let mut bytes = fs::read(&path).unwrap();
    bytes.push(b'\n');
    fs::write(&path, &bytes).unwrap();
    let native = NativeFake::new();
    assert!(reconcile(&temp.0, &policy(), &native, &control()).is_err());
    assert_eq!(native.media_calls.load(Ordering::Relaxed), 0);
    assert_eq!(fs::read(&path).unwrap(), bytes);
    assert_eq!(fs::read(temp.0.join("prepared.json")).unwrap(), marker);
}
fn captured_current(root: &Path) -> PathBuf {
    attempts_for(&current_root(root).unwrap(), MediaRole::Current)
        .unwrap()
        .1
        .unwrap()
}

#[test]
fn explicit_capture_retains_exact_current_media_before_feed_advances_and_reuses_it_for_new_download(
) {
    let temp = Temp::new();
    let root = temp.0.join("windows-prepared");
    let native = initial_current_native(&temp.0.join("installed"));
    let http = current_http();
    capture_current_only(&root, &policy(), &http, &native, &control()).unwrap();
    assert!(!root.exists());
    let current = captured_current(&root);
    assert!(current.join("current.json").is_file());
    assert!(!current.join("prepared.json").exists());
    assert_eq!(native.current_media_calls.load(Ordering::Relaxed), 1);
    assert_eq!(native.new_main_calls.load(Ordering::Relaxed), 0);
    assert_eq!(http.calls.lock().unwrap().len(), 3);
    assert!(descriptor(
        &small(
            &Held::open(&current.join("selected.appinstaller"), SMALL, &control()).unwrap(),
            &control()
        )
        .unwrap(),
        &policy(),
        &source()
    )
    .is_err());
    // Only the private CURRENT consumer admits equality. A NEW cache cannot
    // reinterpret a current marker or its JSON as a newer prepared package.
    assert!(read_prepared(&current, &policy(), &control()).is_err());
    fs::create_dir(&root).unwrap();
    let newer = HttpFake::new();
    let prepared = acquire_with_current(&root, &policy(), &newer, &native, &control()).unwrap();
    assert_eq!(prepared, receipt());
    assert_eq!(native.current_media_calls.load(Ordering::Relaxed), 2);
    assert_eq!(native.new_main_calls.load(Ordering::Relaxed), 1);
    assert_eq!(
        *newer.calls.lock().unwrap(),
        vec![
            policy().discovery,
            new_descriptor(),
            "https://example.invalid/windows/PeerBrush-1.30.1.0.msix".to_string(),
            policy().runtime_uri
        ]
    );
    assert_eq!(
        fs::read(current.join("main.msix")).unwrap(),
        b"main-native-bytes"
    );
    assert!(!current.join("stage-intent.json").exists());
}

#[test]
fn changed_immutable_association_refuses_initial_capture_before_package_io_and_controller_reports_it(
) {
    let temp = Temp::new();
    let root = temp.0.join("windows-prepared");
    let native = initial_current_native(&temp.0.join("installed"));
    let http = HttpFake::new();
    assert_eq!(
        capture_current_only(&root, &policy(), &http, &native, &control()).unwrap_err(),
        CURRENT_MISSING
    );
    assert_eq!(*http.calls.lock().unwrap(), vec![source().association]);
    assert_eq!(native.current_media_calls.load(Ordering::Relaxed), 0);
    assert_eq!(native.new_main_calls.load(Ordering::Relaxed), 0);
    assert!(!root.exists());
    let mut controller = Controller::load(&temp.0);
    assert!(controller.start_current_capture().is_err());
    controller
        .launch_task(WorkKind::Capture, |_| Err(CURRENT_MISSING.into()))
        .unwrap();
    wait_controller(&mut controller, |c| !c.view.busy);
    assert_eq!(controller.view.message, CURRENT_MISSING);
    assert!(controller.view.version.is_none());
    assert!(controller.view.staged_version.is_none());
    assert!(controller.view.observed_target.is_none());
    assert!(!ENABLED);
    assert!(configured().is_none());
}

struct CurrentDuringNew<'a> {
    http: HttpFake,
    current: &'a Path,
    cancel: Option<Arc<AtomicBool>>,
    tamper: bool,
    late_source: Option<&'a AtomicUsize>,
}
impl Transport for CurrentDuringNew<'_> {
    fn get(&self, uri: &str) -> Result<Response> {
        if uri.ends_with("PeerBrush-1.30.1.0.msix") {
            let cache = self.current.parent().unwrap();
            assert!(crate::file_safety::Lease::acquire(&cache.join("acquisition.json")).is_err());
            let media = self.current.join("main.msix");
            if self.tamper {
                #[cfg(windows)]
                assert!(fs::write(&media, b"fake-native-bytes").is_err());
                #[cfg(not(windows))]
                fs::write(&media, b"fake-native-bytes").unwrap();
            }
            if let Some(cancel) = &self.cancel {
                cancel.store(true, Ordering::Release);
            }
            if let Some(source_calls) = self.late_source {
                source_calls.store(usize::MAX, Ordering::Relaxed);
            }
        }
        self.http.get(uri)
    }
}

#[test]
fn current_archive_is_held_through_new_transfer_and_cancel_source_changes_and_tampering_refuse_reuse(
) {
    for fault in [
        "cancel",
        "source",
        "late-source",
        "signature",
        "signed-build",
        "signed-executable",
        "signed-source",
        "tamper",
    ] {
        let temp = Temp::new();
        let root = temp.0.join("windows-prepared");
        let mut native = initial_current_native(&temp.0.join("installed"));
        capture_current_only(&root, &policy(), &current_http(), &native, &control()).unwrap();
        let current = captured_current(&root);
        let marker = fs::read(current.join("current.json")).unwrap();
        fs::create_dir(&root).unwrap();
        let control = control();
        if fault == "source" {
            native.current.build = "9".repeat(40);
        }
        if fault.starts_with("signed-") || fault == "signature" {
            native.archive_fault = fault;
        }
        let http = CurrentDuringNew {
            http: HttpFake::new(),
            current: &current,
            cancel: (fault == "cancel").then(|| control.cancelled.clone()),
            tamper: fault == "tamper",
            late_source: (fault == "late-source").then_some(&native.source_calls),
        };
        let result = acquire_with_current(&root, &policy(), &http, &native, &control);
        if fault == "tamper" && cfg!(windows) {
            assert_eq!(result.unwrap(), receipt());
        } else {
            assert!(result.is_err(), "{fault}");
        }
        assert_eq!(fs::read(current.join("current.json")).unwrap(), marker);
        assert!(!current.join("stage-intent.json").exists());
        assert!(crate::file_safety::Lease::acquire(
            &current_root(&root).unwrap().join("acquisition.json")
        )
        .is_ok());
    }
}

#[test]
fn initial_current_capture_preserves_strict_transport_and_native_failures_and_rejects_forged_cache_claims(
) {
    for fault in [
        "length",
        "truncated",
        "signature",
        "manifest-version",
        "full-name",
        "executing",
        "journal",
    ] {
        let temp = Temp::new();
        let root = temp.0.join("windows-prepared");
        let mut native = initial_current_native(&temp.0.join("installed"));
        let mut http = current_http();
        let uri = "https://example.invalid/windows/PeerBrush-1.30.0.0.msix";
        if fault == "length" {
            http.files.get_mut(uri).unwrap().1 = PACKAGE + 1;
        }
        if fault == "truncated" {
            http.files.get_mut(uri).unwrap().1 += 1;
        }
        if ["signature", "manifest-version", "full-name"].contains(&fault) {
            native.archive_fault = fault;
        }
        if fault == "executing" {
            fs::write(&native.executing, b"foreign-executable").unwrap();
        }
        if fault == "journal" {
            capture_current_only(&root, &policy(), &http, &native, &control()).unwrap();
            let current = captured_current(&root);
            let mut record: Prepared =
                serde_json::from_slice(&fs::read(current.join("current.json")).unwrap()).unwrap();
            record.source.build = "9".repeat(40);
            fs::write(
                current.join("current.json"),
                serde_json::to_vec(&record).unwrap(),
            )
            .unwrap();
        }
        assert!(
            capture_current_only(&root, &policy(), &http, &native, &control()).is_err(),
            "{fault}"
        );
        assert!(!root.exists());
        assert_eq!(native.new_main_calls.load(Ordering::Relaxed), 0);
    }
}

struct CurrentGuardedStage<'a> {
    stage: StageFake,
    current: &'a Path,
    cancel: Option<Arc<AtomicBool>>,
    tamper: bool,
}
impl Stager for CurrentGuardedStage<'_> {
    fn submit(
        &self,
        selected: &RetainedSelection,
        view: Arc<Mutex<View>>,
    ) -> Result<Box<dyn StageOperation>> {
        assert!(crate::file_safety::Lease::acquire(
            &self.current.parent().unwrap().join("acquisition.json")
        )
        .is_err());
        let file = self.current.join("main.msix");
        if self.tamper {
            #[cfg(windows)]
            assert!(fs::write(&file, b"fake-native-bytes").is_err());
            #[cfg(not(windows))]
            fs::write(&file, b"fake-native-bytes").unwrap();
        }
        #[cfg(windows)]
        assert!(OpenOptions::new()
            .write(true)
            .open(self.current.join("current.json"))
            .is_err());
        if let Some(cancel) = &self.cancel {
            cancel.store(true, Ordering::Release);
        }
        self.stage.submit(selected, view)
    }
}
#[test]
fn staging_retains_current_archive_until_real_terminal_readback_including_cancel_and_changed_media()
{
    for fault in ["none", "cancel", "tamper"] {
        let temp = Temp::new();
        let root = temp.0.join("windows-prepared");
        let native = initial_current_native(&temp.0.join("installed"));
        capture_current_only(&root, &policy(), &current_http(), &native, &control()).unwrap();
        let current = captured_current(&root);
        let current_record = fs::read(current.join("current.json")).unwrap();
        fs::create_dir(&root).unwrap();
        acquire_with_current(&root, &policy(), &HttpFake::new(), &native, &control()).unwrap();
        let control = control();
        let (tx, rx) = mpsc::channel();
        tx.send(()).unwrap();
        let submissions = Arc::new(AtomicUsize::new(0));
        let cancellations = Arc::new(AtomicUsize::new(0));
        let stager = CurrentGuardedStage {
            stage: StageFake {
                release: Mutex::new(Some(rx)),
                uncertain: false,
                submissions: submissions.clone(),
                cancellations: cancellations.clone(),
            },
            current: &current,
            cancel: (fault == "cancel").then(|| control.cancelled.clone()),
            tamper: fault == "tamper",
        };
        let result = stage_with_current(&root, &policy(), &native, &stager, &control);
        if fault == "tamper" && !cfg!(windows) {
            assert!(result.is_err());
        } else {
            assert_eq!(result.unwrap().accepted, fault != "cancel");
        }
        assert_eq!(submissions.load(Ordering::Relaxed), 1);
        assert_eq!(
            cancellations.load(Ordering::Relaxed),
            usize::from(fault == "cancel")
        );
        let selected = stage_attempt(&root);
        assert!(selected.join("stage-intent.json").is_file());
        assert!(selected.join("stage-outcome.json").is_file());
        assert_eq!(
            fs::read(current.join("current.json")).unwrap(),
            current_record
        );
        assert!(!current.join("stage-intent.json").exists());
        assert!(crate::file_safety::Lease::acquire(
            &current_root(&root).unwrap().join("acquisition.json")
        )
        .is_ok());
    }
}

#[test]
fn current_cache_reopen_rejects_same_size_restamped_or_replaced_archive_before_new_download() {
    for replacement in [false, true] {
        let temp = Temp::new();
        let root = temp.0.join("windows-prepared");
        let native = initial_current_native(&temp.0.join("installed"));
        capture_current_only(&root, &policy(), &current_http(), &native, &control()).unwrap();
        let current = captured_current(&root);
        let file = current.join("main.msix");
        let original = fs::read(&file).unwrap();
        let modified = fs::metadata(&file).unwrap().modified().unwrap();
        if replacement {
            fs::rename(&file, current.join("retained-original.msix")).unwrap();
        }
        let mut changed = original.clone();
        changed[0] ^= 1;
        fs::write(&file, changed).unwrap();
        OpenOptions::new()
            .write(true)
            .open(&file)
            .unwrap()
            .set_times(fs::FileTimes::new().set_modified(modified))
            .unwrap();
        assert_eq!(fs::metadata(&file).unwrap().len(), original.len() as u64);
        assert_eq!(fs::metadata(&file).unwrap().modified().unwrap(), modified);
        fs::create_dir(&root).unwrap();
        let http = HttpFake::new();
        let native_calls = native.current_media_calls.load(Ordering::Relaxed);
        assert!(acquire_with_current(&root, &policy(), &http, &native, &control()).is_err());
        assert_eq!(
            native.current_media_calls.load(Ordering::Relaxed),
            native_calls
        );
        assert!(http.calls.lock().unwrap().is_empty());
        assert!(attempts(&root).unwrap().0.is_empty());
    }
}

#[test]
fn explicit_copy_worker_reports_real_capture_without_prepared_update_or_health_authority() {
    let temp = Temp::new();
    let root = temp.0.join("windows-prepared");
    let worker_root = root.clone();
    let native = initial_current_native(&temp.0.join("installed"));
    let mut controller = Controller::load(&temp.0);
    controller
        .launch_task(WorkKind::Capture, move |control| {
            capture_current_only(&worker_root, &policy(), &current_http(), &native, control)?;
            Ok(Completed::CurrentArchive)
        })
        .unwrap();
    wait_controller(&mut controller, |c| !c.view.busy);
    assert!(controller
        .view
        .message
        .starts_with("Current-version copy retained."));
    assert!(controller
        .view
        .message
        .contains("Installation and recovery remain unavailable"));
    assert!(controller.view.version.is_none());
    assert!(controller.view.staged_version.is_none());
    assert!(controller.view.observed_target.is_none());
    assert!(matches!(controller.deployment, DeploymentState::Unchecked));
    assert!(captured_current(&root).join("current.json").is_file());
    assert!(!ENABLED);
    assert!(configured().is_none());
}
