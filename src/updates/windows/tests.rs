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
        descriptor: "https://example.invalid/windows/current.appinstaller".into(),
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
        association: policy().descriptor,
        build: "1".repeat(40),
    }
}
fn xml() -> Vec<u8> {
    let p = policy();
    format!("<?xml version=\"1.0\" encoding=\"utf-8\"?><AppInstaller xmlns=\"http://schemas.microsoft.com/appx/appinstaller/2017\" Version=\"1.30.1.0\" Uri=\"{}\"><MainPackage Name=\"{}\" Publisher=\"{}\" Version=\"1.30.1.0\" ProcessorArchitecture=\"x64\" Uri=\"https://example.invalid/windows/PeerBrush-1.30.1.0.msix\"/><Dependencies><Package Name=\"{}\" Publisher=\"{}\" Version=\"14.0.30704.0\" ProcessorArchitecture=\"x64\" Uri=\"{}\"/></Dependencies></AppInstaller>", p.descriptor, p.name, p.publisher, p.runtime_name, p.runtime_publisher, p.runtime_uri).into_bytes()
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
            (policy().descriptor, xml()),
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
    ) -> Result<()> {
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
        Ok(())
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
    assert_eq!(transport.calls.lock().unwrap().len(), 3);
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
        let (_, declared) = transport.files.get_mut(&policy().descriptor).unwrap();
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
    let retained = fs::read(temp.0.join("selected.appinstaller")).unwrap();
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
        fs::read(temp.0.join("selected.appinstaller")).unwrap(),
        retained
    );
}

#[test]
fn interrupted_acquisition_retries_only_in_fresh_owned_attempt_and_reopens_without_get() {
    let temp = Temp::new();
    let mut interrupted = HttpFake::new();
    interrupted.files.get_mut(&policy().descriptor).unwrap().1 += 1;
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
    assert_eq!(transport.calls.lock().unwrap().len(), 3);
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
    interrupted.files.get_mut(&policy().descriptor).unwrap().1 += 1;
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
    assert_eq!(interrupted.calls.lock().unwrap().len(), 3);
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
        ) -> Result<()> {
            NativeFake::new().media(file, policy, selection, runtime, c)?;
            if runtime {
                c.cancelled.store(true, Ordering::Release);
            }
            Ok(())
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
