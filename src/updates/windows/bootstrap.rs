//! Fixed-offer fresh setup. Registration proves identity, never GUI acceptance.
//! No disk receipt authorizes a retry, rollback, uninstall or updater activation.
use super::*;
use std::collections::BTreeSet;

const FORMAT: &str = "PeerBrushWindowsSetupOfferV1";
const INSTALL_ENABLED: bool = false;
const HOLD: &str = "Windows installation is held. Retain the setup files for review; do not retry an interrupted installation.";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Offer {
    schema: u32,
    format: String,
    commit: String,
    version: String,
    policy: Policy,
    selection: Selection,
    descriptor: Media,
    package: Media,
    runtime: Media,
    target: Target,
}
fn media(value: &Media, limit: u64) -> bool {
    value.bytes > 0 && value.bytes <= limit && sha(&value.sha256)
}
fn canonical(value: &impl Serialize) -> Result<Vec<u8>> {
    let value = serde_json::to_value(value).map_err(|_| HOLD)?;
    serde_json::to_vec(&value).map_err(|_| HOLD.into())
}
impl Offer {
    fn parse(bytes: &[u8]) -> Result<Self> {
        if bytes.is_empty() || bytes.len() as u64 > SMALL {
            return Err(HOLD.into());
        }
        let offer: Self = serde_json::from_slice(bytes).map_err(|_| HOLD)?;
        offer.validate()?;
        if canonical(&offer)? != bytes {
            return Err("Setup requires its exact canonical public offer.".into());
        }
        Ok(offer)
    }
    fn validate(&self) -> Result<()> {
        self.policy.validate()?;
        let version = semver::Version::parse(&self.version).map_err(|_| HOLD)?;
        // Current native source/payload layout supports stable 0.x only.
        let expected = [1, version.minor as u16, version.patch as u16, 0];
        let suffix = self
            .policy
            .family
            .strip_prefix(&(self.policy.name.clone() + "_"));
        if self.schema != 1
            || self.format != FORMAT
            || version.to_string() != self.version
            || version.major != 0
            || version.minor > u16::MAX as u64
            || version.patch > u16::MAX as u64
            || !version.pre.is_empty()
            || !version.build.is_empty()
            || self.commit.len() != 40
            || !self
                .commit
                .bytes()
                .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
            || self.selection.version != expected
            || self.target.version != expected
            || self.target.build != self.commit
            || suffix.is_none_or(|s| {
                s.len() != 13
                    || !s
                        .bytes()
                        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
            })
            || self.target.full_name
                != format!(
                    "{}_{}_x64__{}",
                    self.policy.name,
                    version_text(expected),
                    suffix.unwrap_or_default()
                )
            || self.target.source_name != format!("PeerBrush-{}-source.zip", self.version)
            || self.selection.descriptor != self.policy.direct_descriptor(expected)
            || self.selection.runtime_version < [14, 0, 30704, 0]
            || !media(&self.descriptor, SMALL)
            || !media(&self.package, PACKAGE)
            || !media(&self.runtime, PACKAGE)
            || !media(&self.target.executable, PACKAGE)
            || !media(&self.target.source, PACKAGE)
        {
            return Err(
                "Setup offer differs from its stable source, identity or bounded media.".into(),
            );
        }
        let package = self.policy.archive_uri(&self.selection.package)?;
        let descriptor = self.policy.archive_uri(&self.selection.descriptor)?;
        if !package.path().ends_with(".msix")
            || package == descriptor
            || package.path().rsplit_once('/').map(|p| p.0)
                != descriptor.path().rsplit_once('/').map(|p| p.0)
        {
            return Err(HOLD.into());
        }
        Ok(())
    }
    fn digest(&self) -> Result<String> {
        Ok(hash(&canonical(self)?))
    }
    fn matches_build(&self, build: &serde_json::Value) -> bool {
        build["product"] == "PeerBrush"
            && build["tree"] == "clean"
            && build["commit"] == self.commit
            && build["version"] == self.version
    }
}

struct Files {
    roots: Vec<Directory>,
    root: PathBuf,
    offer: Held,
    descriptor: Held,
    package: Held,
    runtime: Held,
    intent: Option<Held>,
    result: Option<Held>,
}
fn save(path: &Path, bytes: &[u8], control: &Control) -> Result<Held> {
    if bytes.is_empty() || bytes.len() as u64 > SMALL {
        return Err(HOLD.into());
    }
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.share_mode(0);
    }
    let mut file = options.open(path).map_err(|_| HOLD)?;
    let identity = crate::file_safety::identity(&file)?;
    file.write_all(bytes)
        .and_then(|_| file.sync_all())
        .map_err(|_| HOLD)?;
    drop(file);
    crate::server::sync_parent(path)?;
    let held = Held::open(path, SMALL, control)?;
    if held.identity != identity || small(&held, control)? != bytes {
        return Err(HOLD.into());
    }
    Ok(held)
}
impl Files {
    fn prepare(
        root: &Path,
        offer: &Offer,
        transport: &dyn Transport,
        control: &Control,
    ) -> Result<Self> {
        offer.validate()?;
        if !root.is_absolute()
            || root.components().any(|c| {
                matches!(
                    c,
                    std::path::Component::CurDir | std::path::Component::ParentDir
                )
            })
        {
            return Err(HOLD.into());
        }
        #[cfg(windows)]
        if !matches!(root.components().next(), Some(std::path::Component::Prefix(p)) if matches!(p.kind(), std::path::Prefix::Disk(_) | std::path::Prefix::VerbatimDisk(_)))
        {
            return Err(HOLD.into());
        }
        // Pin ancestors before creating descendants; never follow a junction.
        let mut ancestors: Vec<_> = root.parent().ok_or(HOLD)?.ancestors().collect();
        if ancestors.len() > 64 {
            return Err(HOLD.into());
        }
        ancestors.reverse();
        let mut roots = vec![];
        for path in ancestors {
            match fs::symlink_metadata(path) {
                Ok(_) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    fs::create_dir(path).map_err(|_| HOLD)?;
                }
                Err(_) => return Err(HOLD.into()),
            }
            roots.push(Directory::open(path)?);
        }
        // shortcut: a prior setup attempt always holds, even after terminal
        // success. Add authenticated interruption/retirement qualification before
        // supporting retry/reinstall; never interpret ready JSON as authority.
        fs::create_dir(root).map_err(|_| HOLD)?;
        roots.push(Directory::open(root)?);
        let held_offer = save(&root.join("offer.json"), &canonical(offer)?, control)?;
        let descriptor = download(
            transport,
            &offer.selection.descriptor,
            &root.join("discovery.appinstaller"),
            SMALL,
            control,
            "Downloading fixed installation metadata",
        )?;
        if descriptor.media != offer.descriptor
            || descriptor_content(&small(&descriptor, control)?, &offer.policy, None)?
                != offer.selection
        {
            return Err("Setup discovery bytes differ from the fixed offer.".into());
        }
        let package = download(
            transport,
            &offer.selection.package,
            &root.join("package.msix"),
            PACKAGE,
            control,
            "Downloading exact installation package",
        )?;
        if package.media != offer.package {
            return Err("Setup package differs from its fixed digest.".into());
        }
        let runtime = download(
            transport,
            &offer.policy.runtime_uri,
            &root.join("runtime.appx"),
            PACKAGE,
            control,
            "Downloading exact runtime dependency",
        )?;
        let files = Self {
            roots,
            root: root.into(),
            offer: held_offer,
            descriptor,
            package,
            runtime,
            intent: None,
            result: None,
        };
        files.check(offer, control)?;
        Ok(files)
    }
    fn check(&self, offer: &Offer, control: &Control) -> Result<()> {
        for root in &self.roots {
            root.check()?;
        }
        for held in [&self.offer, &self.descriptor, &self.package, &self.runtime]
            .into_iter()
            .chain(self.intent.iter())
            .chain(self.result.iter())
        {
            held.check(control)?;
        }
        if small(&self.offer, control)? != canonical(offer)?
            || self.descriptor.media != offer.descriptor
            || self.package.media != offer.package
            || self.runtime.media != offer.runtime
            || descriptor_content(&small(&self.descriptor, control)?, &offer.policy, None)?
                != offer.selection
        {
            return Err(HOLD.into());
        }
        let mut expected: BTreeSet<_> = [
            "offer.json",
            "discovery.appinstaller",
            "package.msix",
            "runtime.appx",
        ]
        .into_iter()
        .map(String::from)
        .collect();
        if self.intent.is_some() {
            expected.insert("install-intent.json".into());
        }
        if self.result.is_some() {
            expected.insert("install-result.json".into());
        }
        let mut actual = BTreeSet::new();
        for entry in fs::read_dir(&self.root).map_err(|_| HOLD)? {
            let name = entry
                .map_err(|_| HOLD)?
                .file_name()
                .into_string()
                .map_err(|_| HOLD)?;
            if actual.len() >= 6 || !actual.insert(name) {
                return Err(HOLD.into());
            }
        }
        if actual != expected {
            return Err("Unexpected setup inventory; preserve it for review.".into());
        }
        Ok(())
    }
    fn verify(&self, offer: &Offer, verifier: &dyn Verifier, control: &Control) -> Result<()> {
        self.check(offer, control)?;
        if verifier.media(
            &self.package,
            &offer.policy,
            &offer.selection,
            false,
            control,
        )? != Some(offer.target.clone())
            || verifier
                .media(
                    &self.runtime,
                    &offer.policy,
                    &offer.selection,
                    true,
                    control,
                )?
                .is_some()
        {
            return Err(
                "Signed setup payload differs from the fixed source and executable.".into(),
            );
        }
        self.check(offer, control)
    }
}

trait Deployment {
    fn absent(&self, policy: &Policy, control: &Control) -> Result<bool>;
    fn submit(
        &self,
        files: &Files,
        offer: &Offer,
        control: &Control,
        guard: &dyn Fn() -> Result<()>,
    ) -> Result<Box<dyn StageOperation>>;
    fn installed(&self, offer: &Offer, control: &Control) -> Result<bool>;
}
fn drain(operation: &mut dyn StageOperation, control: &Control) -> (StageTerminal, bool) {
    let mut rejected = false;
    let mut canceled = false;
    loop {
        rejected |= control.check().is_err();
        if rejected && !canceled {
            operation.cancel();
            canceled = true;
        }
        match operation.poll() {
            StageObservation::Terminal(terminal) => {
                return (terminal, rejected || control.check().is_err())
            }
            StageObservation::Uncertain => rejected = true,
            StageObservation::Pending => {}
        }
        // Keep the operation and every byte lease until observed native terminal,
        // including after timeout/Cancel. A killed owner leaves the durable hold.
        std::thread::sleep(Duration::from_millis(50));
    }
}
#[derive(Serialize)]
struct Observation<'a> {
    schema: u32,
    format: &'static str,
    offer_sha256: String,
    commit: &'a str,
    version: &'a str,
    package: &'a Media,
    runtime: &'a Media,
    target: &'a Target,
    installed_identity_verified: bool,
    installed_acceptance: &'static str,
    update_eligible: bool,
    ready: bool,
}
fn observe<'a>(
    offer: &'a Offer,
    deployment: &dyn Deployment,
    control: &Control,
) -> Result<Observation<'a>> {
    offer.validate()?;
    let matched = deployment.installed(offer, control)?;
    Ok(Observation {
        schema: 1,
        format: "PeerBrushWindowsInstalledIdentityV1",
        offer_sha256: offer.digest()?,
        commit: &offer.commit,
        version: &offer.version,
        package: &offer.package,
        runtime: &offer.runtime,
        target: &offer.target,
        installed_identity_verified: matched,
        installed_acceptance: "not-established",
        update_eligible: false,
        ready: false,
    })
}
fn install(
    files: &mut Files,
    offer: &Offer,
    verifier: &dyn Verifier,
    deployment: &dyn Deployment,
    control: &Control,
) -> Result<bool> {
    files.verify(offer, verifier, control)?;
    if !deployment.absent(&offer.policy, control)? {
        return Err("An installation already exists. Use its own update channel.".into());
    }
    let intent = serde_json::json!({"schema":1,"format":"PeerBrushWindowsSetupIntentV1","offer_sha256":offer.digest()?,"target":offer.target});
    files.intent = Some(save(
        &files.root.join("install-intent.json"),
        &canonical(&intent)?,
        control,
    )?);
    let guard = || {
        files.verify(offer, verifier, control)?;
        if !deployment.absent(&offer.policy, control)? {
            return Err(HOLD.into());
        }
        Ok(())
    };
    guard()?;
    let mut operation = deployment.submit(files, offer, control, &guard)?;
    let (terminal, rejected) = drain(operation.as_mut(), control);
    drop(operation);
    let readback = Control {
        cancelled: Arc::new(AtomicBool::new(false)),
        deadline: Instant::now() + TIMEOUT,
        view: control.view.clone(),
    };
    files.verify(offer, verifier, &readback)?;
    let observation = observe(offer, deployment, &readback)?;
    let accepted = !rejected
        && terminal.hresult == Some(0)
        && terminal.registered == Some(true)
        && !terminal.canceled
        && !terminal.invalid_progress
        && observation.installed_identity_verified;
    let result = serde_json::json!({"schema":1,"format":"PeerBrushWindowsSetupResultV1","intent_sha256":files.intent.as_ref().ok_or(HOLD)?.media.sha256,
        "terminal":terminal,"registered_identity_verified":accepted,"observation":observation,"ready":false});
    files.result = Some(save(
        &files.root.join("install-result.json"),
        &canonical(&result)?,
        &readback,
    )?);
    files.check(offer, &readback)?;
    Ok(accepted)
}

// A live production permit can only be created after the compiled offer, source
// and explicit human consent pass. It is never deserialized from a receipt.
#[cfg(windows)]
pub(super) struct Permit {
    _private: (),
}

#[cfg(feature = "windows-setup")]
#[path = "bootstrap/executable.rs"]
mod executable;
#[cfg(feature = "windows-setup")]
pub(super) fn entry(embedded: Option<&str>) -> i32 {
    executable::entry(embedded)
}

#[cfg(test)]
#[path = "bootstrap/tests.rs"]
mod tests;
