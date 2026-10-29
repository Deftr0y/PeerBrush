//! Ordering/refusal fixtures only: no packages are installed or signed.
use super::*;
use std::{cell::Cell, collections::BTreeMap, io::Cursor, sync::atomic::AtomicUsize};

struct Temp(PathBuf);
impl Temp {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!("peerbrush-setup-{}", crate::engine::id()));
        fs::create_dir(&root).unwrap();
        Self(fs::canonicalize(root).unwrap())
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        assert_eq!(fs::canonicalize(&self.0).unwrap(), self.0);
        assert!(!fs::symlink_metadata(&self.0)
            .unwrap()
            .file_type()
            .is_symlink());
        fs::remove_dir_all(&self.0).unwrap();
    }
}
fn control() -> Control {
    Control {
        cancelled: Arc::new(AtomicBool::new(false)),
        deadline: Instant::now() + TIMEOUT,
        view: Arc::new(Mutex::new(View::default())),
    }
}
fn measured(bytes: &[u8]) -> Media {
    Media {
        bytes: bytes.len() as u64,
        sha256: hash(bytes),
    }
}
fn fixture() -> (Offer, HttpFixture) {
    let mut policy = super::super::tests::policy();
    policy.family = policy.name.clone() + "_abcdefgh12345";
    let selected = Selection {
        descriptor: policy.direct_descriptor([1, 30, 1, 0]),
        version: [1, 30, 1, 0],
        package: policy.archive_root.clone() + "PeerBrush-1.30.1.0-signed-r1.msix",
        runtime_version: [14, 0, 30704, 0],
    };
    let descriptor = format!("<?xml version=\"1.0\" encoding=\"utf-8\"?><AppInstaller xmlns=\"http://schemas.microsoft.com/appx/appinstaller/2017\" Version=\"1.30.1.0\" Uri=\"{}\"><MainPackage Name=\"{}\" Publisher=\"{}\" Version=\"1.30.1.0\" ProcessorArchitecture=\"x64\" Uri=\"{}\"/><Dependencies><Package Name=\"{}\" Publisher=\"{}\" Version=\"14.0.30704.0\" ProcessorArchitecture=\"x64\" Uri=\"{}\"/></Dependencies></AppInstaller>", selected.descriptor, policy.name, policy.publisher, selected.package, policy.runtime_name, policy.runtime_publisher, policy.runtime_uri).into_bytes();
    let package = b"synthetic signed archive contract".to_vec();
    let runtime = b"synthetic signed runtime contract".to_vec();
    let offer = Offer {
        schema: 1,
        format: FORMAT.into(),
        commit: "2".repeat(40),
        version: "0.30.1".into(),
        target: Target {
            full_name: policy.name.clone() + "_1.30.1.0_x64__abcdefgh12345",
            version: selected.version,
            build: "2".repeat(40),
            executable: measured(b"native executable"),
            source_name: "PeerBrush-0.30.1-source.zip".into(),
            source: measured(b"native source"),
        },
        descriptor: measured(&descriptor),
        package: measured(&package),
        runtime: measured(&runtime),
        policy: policy.clone(),
        selection: selected.clone(),
    };
    let http = HttpFixture {
        files: [
            (selected.descriptor, descriptor),
            (selected.package, package),
            (policy.runtime_uri, runtime),
        ]
        .into_iter()
        .collect(),
        calls: Mutex::new(vec![]),
    };
    (offer, http)
}
struct HttpFixture {
    files: BTreeMap<String, Vec<u8>>,
    calls: Mutex<Vec<String>>,
}
impl Transport for HttpFixture {
    fn get(&self, uri: &str) -> Result<Response> {
        self.calls.lock().unwrap().push(uri.into());
        let bytes = self.files.get(uri).ok_or("No fixture response")?.clone();
        Ok(Response {
            length: bytes.len() as u64,
            reader: Box::new(Cursor::new(bytes)),
        })
    }
}
struct VerifierFixture {
    target: Target,
    reject_runtime: bool,
    calls: Cell<usize>,
}
impl VerifierFixture {
    fn new(offer: &Offer) -> Self {
        Self {
            target: offer.target.clone(),
            reject_runtime: false,
            calls: Cell::new(0),
        }
    }
}
impl Verifier for VerifierFixture {
    fn source(&self, _: &Policy) -> Result<Source> {
        panic!("Fresh setup must not fabricate a current installed source")
    }
    fn media(
        &self,
        file: &Held,
        _: &Policy,
        _: &Selection,
        runtime: bool,
        control: &Control,
    ) -> Result<Option<Target>> {
        file.check(control)?;
        self.calls.set(self.calls.get() + 1);
        if runtime && self.reject_runtime {
            return Err("Synthetic signature refusal".into());
        }
        Ok((!runtime).then(|| self.target.clone()))
    }
}
struct HostFixture {
    existing: Cell<bool>,
    matches: bool,
    late_foreign: bool,
    uncertain: bool,
    cancel_at_terminal: bool,
    succeeded: bool,
    submitted: Cell<usize>,
    polls: Arc<AtomicUsize>,
    canceled: Arc<AtomicUsize>,
}
impl HostFixture {
    fn new() -> Self {
        Self {
            existing: Cell::new(false),
            matches: true,
            late_foreign: false,
            uncertain: false,
            cancel_at_terminal: false,
            succeeded: true,
            submitted: Cell::new(0),
            polls: Arc::new(AtomicUsize::new(0)),
            canceled: Arc::new(AtomicUsize::new(0)),
        }
    }
}
struct OperationFixture {
    polls: Arc<AtomicUsize>,
    canceled: Arc<AtomicUsize>,
    uncertain: bool,
    succeeded: bool,
    control: Option<Arc<AtomicBool>>,
    finished: bool,
}
impl StageOperation for OperationFixture {
    fn poll(&mut self) -> StageObservation {
        let call = self.polls.fetch_add(1, Ordering::AcqRel);
        if self.uncertain && call < 2 {
            return if call == 0 {
                StageObservation::Uncertain
            } else {
                StageObservation::Pending
            };
        }
        if let Some(cancel) = &self.control {
            cancel.store(true, Ordering::Release);
        }
        self.finished = true;
        StageObservation::Terminal(StageTerminal {
            hresult: Some(if self.succeeded { 0 } else { -1 }),
            registered: Some(self.succeeded),
            canceled: false,
            invalid_progress: false,
        })
    }
    fn cancel(&mut self) {
        self.canceled.fetch_add(1, Ordering::AcqRel);
    }
}
impl Drop for OperationFixture {
    fn drop(&mut self) {
        assert!(
            self.finished,
            "Never drop an unobserved operation and its media leases"
        );
    }
}
impl Deployment for HostFixture {
    fn absent(&self, _: &Policy, _: &Control) -> Result<bool> {
        Ok(!self.existing.get())
    }
    fn submit(
        &self,
        files: &Files,
        _: &Offer,
        control: &Control,
        guard: &dyn Fn() -> Result<()>,
    ) -> Result<Box<dyn StageOperation>> {
        assert!(files.intent.is_some());
        assert!(files.root.join("install-intent.json").is_file());
        if self.late_foreign {
            fs::write(files.root.join("foreign"), b"preserve foreign replacement").unwrap();
        }
        guard()?;
        self.submitted.set(self.submitted.get() + 1);
        Ok(Box::new(OperationFixture {
            polls: self.polls.clone(),
            canceled: self.canceled.clone(),
            uncertain: self.uncertain,
            succeeded: self.succeeded,
            control: self.cancel_at_terminal.then(|| control.cancelled.clone()),
            finished: false,
        }))
    }
    fn installed(&self, _: &Offer, _: &Control) -> Result<bool> {
        Ok(self.matches)
    }
}

#[test]
fn fixed_offer_rejects_unknown_duplicate_noncanonical_and_mismatched_bindings() {
    let (offer, _) = fixture();
    let bytes = canonical(&offer).unwrap();
    assert_eq!(Offer::parse(&bytes).unwrap(), offer);
    assert!(Offer::parse(&[bytes.clone(), b"\n".to_vec()].concat()).is_err());
    assert!(Offer::parse(&vec![b' '; SMALL as usize + 1]).is_err());
    let duplicate = format!(
        "{{\"schema\":1,{}",
        std::str::from_utf8(&bytes).unwrap().trim_start_matches('{')
    );
    assert!(Offer::parse(duplicate.as_bytes()).is_err());
    let cases = [
        ("schema", serde_json::json!(true)),
        ("version", serde_json::json!("0.30.1-dev.1")),
        ("commit", serde_json::json!("unknown")),
        ("unknown", serde_json::json!("not a field")),
        ("target", serde_json::json!({})),
    ];
    for (field, value) in cases {
        let mut bad = serde_json::to_value(&offer).unwrap();
        bad[field] = value;
        assert!(Offer::parse(&canonical(&bad).unwrap()).is_err(), "{field}");
    }
    for field in ["descriptor", "package", "runtime"] {
        let mut bad = serde_json::to_value(&offer).unwrap();
        bad[field]["bytes"] = serde_json::json!(0);
        assert!(Offer::parse(&canonical(&bad).unwrap()).is_err());
    }
    let mut bad = offer.clone();
    bad.target.build = "3".repeat(40);
    assert!(bad.validate().is_err());
    let mut bad = offer.clone();
    bad.selection.descriptor = bad.policy.discovery.clone();
    assert!(bad.validate().is_err());
    let mut bad = offer;
    bad.target.source_name = "foreign-source.zip".into();
    assert!(bad.validate().is_err());
}

#[test]
fn signed_media_intent_native_terminal_and_installed_identity_are_all_required() {
    let temp = Temp::new();
    let (offer, http) = fixture();
    let c = control();
    let mut files = Files::prepare(&temp.0.join("setup"), &offer, &http, &c).unwrap();
    let verifier = VerifierFixture::new(&offer);
    let host = HostFixture::new();
    assert!(install(&mut files, &offer, &verifier, &host, &c).unwrap());
    assert_eq!(host.submitted.get(), 1);
    assert!(verifier.calls.get() >= 6);
    let result: serde_json::Value =
        serde_json::from_slice(&small(files.result.as_ref().unwrap(), &c).unwrap()).unwrap();
    assert_eq!(result["registered_identity_verified"], true);
    assert_eq!(
        result["observation"]["installed_acceptance"],
        "not-established"
    );
    assert_eq!(result["observation"]["update_eligible"], false);
    assert_eq!(result["ready"], false);
    let previous = fs::read(files.root.join("install-result.json")).unwrap();
    assert!(install(&mut files, &offer, &verifier, &host, &c).is_err());
    assert_eq!(host.submitted.get(), 1);
    assert_eq!(
        fs::read(files.root.join("install-result.json")).unwrap(),
        previous
    );
}

#[test]
fn altered_discovery_archive_and_runtime_signatures_refuse_before_deployment() {
    for bad in ["descriptor", "package", "runtime-signature", "target"] {
        let temp = Temp::new();
        let (offer, mut http) = fixture();
        let c = control();
        if bad == "descriptor" {
            http.files
                .get_mut(&offer.selection.descriptor)
                .unwrap()
                .push(b' ');
        }
        if bad == "package" {
            http.files.get_mut(&offer.selection.package).unwrap()[0] ^= 1;
        }
        let prepared = Files::prepare(&temp.0.join("setup"), &offer, &http, &c);
        if matches!(bad, "descriptor" | "package") {
            assert!(prepared.is_err());
            continue;
        }
        let mut files = prepared.unwrap();
        let mut verifier = VerifierFixture::new(&offer);
        if bad == "runtime-signature" {
            verifier.reject_runtime = true;
        } else {
            verifier.target.build = "4".repeat(40);
        }
        let host = HostFixture::new();
        assert!(install(&mut files, &offer, &verifier, &host, &c).is_err());
        assert_eq!(host.submitted.get(), 0);
        assert!(!files.root.join("install-intent.json").exists());
    }
}

#[test]
fn existing_registration_and_late_foreign_inventory_never_submit() {
    for late in [false, true] {
        let temp = Temp::new();
        let (offer, http) = fixture();
        let c = control();
        let mut files = Files::prepare(&temp.0.join("setup"), &offer, &http, &c).unwrap();
        let verifier = VerifierFixture::new(&offer);
        let mut host = HostFixture::new();
        host.existing.set(!late);
        host.late_foreign = late;
        assert!(install(&mut files, &offer, &verifier, &host, &c).is_err());
        assert_eq!(host.submitted.get(), 0);
        if late {
            assert_eq!(
                fs::read(files.root.join("foreign")).unwrap(),
                b"preserve foreign replacement"
            );
        }
    }
}

#[test]
fn uncertain_cancel_late_terminal_failed_provider_and_wrong_identity_never_grant_acceptance() {
    for failure in ["uncertain", "late-cancel", "provider", "installed-identity"] {
        let temp = Temp::new();
        let (offer, http) = fixture();
        let c = control();
        let mut files = Files::prepare(&temp.0.join("setup"), &offer, &http, &c).unwrap();
        let verifier = VerifierFixture::new(&offer);
        let mut host = HostFixture::new();
        host.uncertain = failure == "uncertain";
        host.cancel_at_terminal = failure == "late-cancel";
        host.succeeded = failure != "provider";
        host.matches = failure != "installed-identity";
        assert!(
            !install(&mut files, &offer, &verifier, &host, &c).unwrap(),
            "{failure}"
        );
        assert_eq!(host.submitted.get(), 1);
        assert!(files.result.is_some());
        if host.uncertain {
            assert_eq!(host.polls.load(Ordering::Acquire), 3);
            assert_eq!(host.canceled.load(Ordering::Acquire), 1);
        }
    }
}

#[test]
fn prior_attempts_and_foreign_destinations_are_preserved_and_never_replayed() {
    let temp = Temp::new();
    let (offer, http) = fixture();
    let c = control();
    let root = temp.0.join("existing");
    fs::create_dir(&root).unwrap();
    fs::write(root.join("foreign"), b"foreign bytes").unwrap();
    assert!(Files::prepare(&root, &offer, &http, &c).is_err());
    assert_eq!(fs::read(root.join("foreign")).unwrap(), b"foreign bytes");
    let files = Files::prepare(&temp.0.join("fresh"), &offer, &http, &c).unwrap();
    assert!(Files::prepare(&files.root, &offer, &http, &c).is_err());
    files.check(&offer, &c).unwrap();
}

#[test]
fn read_only_identity_proof_cannot_grant_installed_acceptance_or_update_authority() {
    let (offer, _) = fixture();
    let host = HostFixture::new();
    let result = observe(&offer, &host, &control()).unwrap();
    assert!(result.installed_identity_verified);
    assert!(!result.update_eligible);
    assert!(!result.ready);
    assert_eq!(host.submitted.get(), 0);
    assert_eq!(result.installed_acceptance, "not-established");
}

#[cfg(windows)]
#[test]
fn native_setup_media_leases_deny_write_and_delete_until_the_owner_finishes() {
    let temp = Temp::new();
    let (offer, http) = fixture();
    let c = control();
    let files = Files::prepare(&temp.0.join("setup"), &offer, &http, &c).unwrap();
    for held in [
        &files.offer,
        &files.descriptor,
        &files.package,
        &files.runtime,
    ] {
        assert!(OpenOptions::new().write(true).open(&held.path).is_err());
        assert!(fs::remove_file(&held.path).is_err());
        held.check(&c).unwrap();
    }
}

#[cfg(all(windows, feature = "windows-setup"))]
#[test]
#[ignore = "Requires separately approved genuine signed packages in a disposable hosted Windows fixture"]
fn genuine_fixed_offer_fresh_install_in_explicit_disposable_hosted_profile() {
    assert!(
        std::env::var("PEERBRUSH_SETUP_FIXTURE_APPROVED").as_deref()
            == Ok("install-selected-signed-package")
    );
    assert!(std::env::var("GITHUB_ACTIONS").as_deref() == Ok("true"));
    assert!(std::env::var("RUNNER_ENVIRONMENT").as_deref() == Ok("github-hosted"));
    #[link(name = "advapi32")]
    extern "system" {
        fn GetUserNameW(buffer: *mut u16, size: *mut u32) -> i32;
    }
    let mut buffer = [0u16; 256];
    let mut size = buffer.len() as u32;
    assert!(
        unsafe { GetUserNameW(buffer.as_mut_ptr(), &mut size) } != 0 && size > 1 && size <= 256
    );
    assert!(
        String::from_utf16(&buffer[..size as usize - 1])
            .unwrap()
            .eq_ignore_ascii_case("runneradmin"),
        "Requires the disposable hosted account"
    );
    let c = control();
    let input = PathBuf::from(
        std::env::var_os("PEERBRUSH_SETUP_FIXTURE_OFFER")
            .expect("Explicit public offer path required"),
    );
    let checksum = std::env::var("PEERBRUSH_SETUP_FIXTURE_OFFER_SHA256")
        .expect("Independent public offer digest required");
    assert!(sha(&checksum));
    let held = Held::open(&input, SMALL, &c).unwrap();
    assert_eq!(held.media.sha256, checksum);
    let offer = Offer::parse(&small(&held, &c).unwrap()).unwrap();
    let output = PathBuf::from(
        std::env::var_os("PEERBRUSH_SETUP_FIXTURE_OUTPUT")
            .expect("Fresh private fixture output required"),
    );
    let temp = fs::canonicalize(std::env::temp_dir()).unwrap();
    assert!(fs::canonicalize(output.parent().unwrap())
        .unwrap()
        .starts_with(temp));
    assert!(super::executable::qualify_fixture(&offer, &output, &c).unwrap());
    held.check(&c).unwrap();
    // Retain all evidence. The host is disposable; this test does not uninstall,
    // erase profiles, declare GUI health or turn production activation on.
}
