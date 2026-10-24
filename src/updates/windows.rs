//! Signed Windows media and dormant native staging. No registration or restart.
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc, Mutex,
    },
    time::{Duration, Instant},
};

#[cfg(windows)]
mod native;
#[cfg(test)]
mod tests;

pub(crate) const ENABLED: bool = false;
const PACKAGE: u64 = 512 * 1024 * 1024;
const SMALL: u64 = 16 * 1024;
const TIMEOUT: Duration = Duration::from_secs(10 * 60);
const DISABLED: &str = "Signed Windows downloads are not configured in this development build.";
const RUNTIME_NAME: &str = "Microsoft.VCLibs.140.00.UWPDesktop";
const RUNTIME_PUBLISHER: &str =
    "CN=Microsoft Corporation, O=Microsoft Corporation, L=Redmond, S=Washington, C=US";
type Result<T> = std::result::Result<T, String>;

// No feed/key/certificate defaults and no user-supplied URL/configuration parser.
fn configured() -> Option<Policy> {
    None
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Policy {
    descriptor: String,
    name: String,
    publisher: String,
    publisher_display: String,
    family: String,
    certificate: String,
    runtime_uri: String,
    runtime_name: String,
    runtime_publisher: String,
    runtime_certificate: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Source {
    full_name: String,
    family: String,
    version: [u16; 4],
    association: String,
    build: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Selection {
    version: [u16; 4],
    package: String,
    runtime_version: [u16; 4],
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Media {
    bytes: u64,
    sha256: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Prepared {
    schema: u32,
    source: Source,
    policy: Policy,
    selection: Selection,
    descriptor: Media,
    package: Media,
    runtime: Media,
}

// Obtained from the native verifier's held signed payload, never from a journal.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Target {
    full_name: String,
    version: [u16; 4],
    build: String,
    executable: Media,
    source_name: String,
    source: Media,
}
impl Target {
    fn matches(&self, current: &Source, policy: &Policy) -> bool {
        current.full_name == self.full_name
            && current.version == self.version
            && current.build == self.build
            && current.family == policy.family
            && current.association == policy.descriptor
    }
}
struct RunningTarget {
    current: Source,
    owned: Directory,
    executable: Held,
    source: Held,
}
impl RunningTarget {
    fn open(
        root: &Path,
        executing: &Path,
        current: Source,
        target: &Target,
        control: &Control,
    ) -> Result<Self> {
        let owned = Directory::open(root)?;
        let executable_path = root.join("peerbrush.exe");
        if fs::canonicalize(executing).map_err(|_| STAGE_HOLD)?
            != fs::canonicalize(&executable_path).map_err(|_| STAGE_HOLD)?
            || target.version[0] != 1
            || target.version[3] != 0
            || target.source_name
                != format!(
                    "PeerBrush-0.{}.{}-source.zip",
                    target.version[1], target.version[2]
                )
        {
            return Err(STAGE_HOLD.into());
        }
        let result = Self {
            current,
            executable: Held::open(&executable_path, PACKAGE, control)?,
            source: Held::open(&root.join(&target.source_name), PACKAGE, control)?,
            owned,
        };
        result.check_files(target, control)?;
        Ok(result)
    }
    fn check_files(&self, target: &Target, control: &Control) -> Result<()> {
        self.owned.check()?;
        self.executable.check(control)?;
        self.source.check(control)?;
        if self.executable.media != target.executable || self.source.media != target.source {
            return Err(STAGE_HOLD.into());
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct View {
    pub busy: bool,
    pub bytes: u64,
    pub phase: &'static str,
    pub message: String,
    pub version: Option<[u16; 4]>,
    pub staged_version: Option<[u16; 4]>,
    pub percentage: Option<u32>,
    pub installed_version: Option<[u16; 4]>,
    pub recorded_staging: bool,
    pub observed_target: Option<[u16; 4]>,
}

struct Control {
    cancelled: Arc<AtomicBool>,
    deadline: Instant,
    view: Arc<Mutex<View>>,
}
impl Control {
    fn check(&self) -> Result<()> {
        if self.cancelled.load(Ordering::Acquire) {
            return Err("Windows download canceled; no package was installed.".into());
        }
        if Instant::now() >= self.deadline {
            return Err("Windows download timed out; no package was installed.".into());
        }
        Ok(())
    }
    fn phase(&self, phase: &'static str, bytes: u64) -> Result<()> {
        self.check()?;
        let mut view = self.view.lock().unwrap();
        view.phase = phase;
        view.bytes = bytes;
        Ok(())
    }
}

struct Response {
    reader: Box<dyn Read + Send>,
    length: u64,
}
trait Transport {
    fn get(&self, uri: &str) -> Result<Response>;
}
trait Verifier {
    fn source(&self, policy: &Policy) -> Result<Source>;
    fn media(
        &self,
        file: &Held,
        policy: &Policy,
        selection: &Selection,
        runtime: bool,
        control: &Control,
    ) -> Result<Option<Target>>;
    // A current archive is verified independently against the running package,
    // never admitted as a newer download/staging target.
    fn current_media(&self, _: &Held, _: &Policy, _: &Source, _: &Control) -> Result<Target> {
        Err(STAGE_HOLD.into())
    }
    fn running_target(&self, _: &Policy, _: &Target, _: &Control) -> Result<RunningTarget> {
        Err(STAGE_HOLD.into())
    }
}

#[cfg(windows)]
struct Http(ureq::Agent);
#[cfg(windows)]
impl Http {
    fn new() -> Self {
        Self(
            ureq::AgentBuilder::new()
                .redirects(0)
                .timeout_connect(Duration::from_secs(5))
                .timeout_read(Duration::from_secs(5))
                .build(),
        )
    }
}
#[cfg(windows)]
impl Transport for Http {
    fn get(&self, uri: &str) -> Result<Response> {
        let response = self
            .0
            .get(uri)
            .set("Accept-Encoding", "identity")
            .set("User-Agent", "PeerBrush signed Windows download")
            .call()
            .map_err(|_| "Signed download could not be retrieved over HTTPS; try again later.")?;
        if response.status() != 200
            || response
                .header("Content-Encoding")
                .is_some_and(|s| s != "identity")
        {
            return Err("Unexpected signed download response.".into());
        }
        let length = response
            .header("Content-Length")
            .and_then(|v| v.parse().ok())
            .ok_or("Signed download has no exact byte length.")?;
        Ok(Response {
            reader: response.into_reader(),
            length,
        })
    }
}

fn https(value: &str) -> Result<url::Url> {
    let uri = url::Url::parse(value).map_err(|_| "Invalid approved Windows media URI.")?;
    if uri.as_str() != value
        || uri.scheme() != "https"
        || uri.host_str().is_none()
        || !uri.username().is_empty()
        || uri.password().is_some()
        || uri.query().is_some()
        || uri.fragment().is_some()
        || uri.port().is_some()
        || value.len() > 4096
        || !value.is_ascii()
        || value
            .bytes()
            .any(|b| b <= 32 || b == 127 || b == b'%' || b == b'\\')
    {
        return Err("Invalid approved Windows media URI.".into());
    }
    Ok(uri)
}
fn hash(bytes: &[u8]) -> String {
    hex(ring::digest::digest(&ring::digest::SHA256, bytes).as_ref())
}
fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
fn sha(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn text(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 1024
        && value
            .bytes()
            .all(|b| (32..127).contains(&b) && !b"&<>\"".contains(&b))
}
fn version(value: &str) -> Result<[u16; 4]> {
    let values = value
        .split('.')
        .map(|s| {
            let v = s
                .parse::<u16>()
                .map_err(|_| "Invalid native package version.")?;
            if s != v.to_string() {
                return Err("Noncanonical native package version.");
            }
            Ok(v)
        })
        .collect::<std::result::Result<Vec<_>, &str>>()?;
    values
        .try_into()
        .map_err(|_| "Invalid native package version.".into())
}
fn version_text(v: [u16; 4]) -> String {
    format!("{}.{}.{}.{}", v[0], v[1], v[2], v[3])
}
impl Policy {
    fn validate(&self) -> Result<()> {
        https(&self.descriptor)?;
        https(&self.runtime_uri)?;
        if !self.descriptor.ends_with(".appinstaller")
            || !self.runtime_uri.ends_with(".appx")
            || !sha(&self.certificate)
            || !sha(&self.runtime_certificate)
            || self.runtime_name != RUNTIME_NAME
            || self.runtime_publisher != RUNTIME_PUBLISHER
            || [
                &self.name,
                &self.publisher,
                &self.publisher_display,
                &self.family,
                &self.runtime_name,
                &self.runtime_publisher,
            ]
            .into_iter()
            .any(|s| !text(s) || s.contains("{{") || s.contains("REPLACE_"))
        {
            return Err("Public Windows distribution policy is incomplete.".into());
        }
        Ok(())
    }
}

fn descriptor(bytes: &[u8], policy: &Policy, source: &Source) -> Result<Selection> {
    policy.validate()?;
    if source.family != policy.family
        || source.association != policy.descriptor
        || source.build.len() != 40
        || !source
            .build
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err("This installed Windows source or association changed.".into());
    }
    if bytes.is_empty() || bytes.len() as u64 > SMALL || !bytes.is_ascii() || bytes.contains(&b'&')
    {
        return Err("Unsupported App Installer descriptor.".into());
    }
    let xml = std::str::from_utf8(bytes).map_err(|_| "Invalid App Installer text.")?;
    let doc = roxmltree::Document::parse(xml).map_err(|_| "Invalid App Installer XML.")?;
    let root = doc.root_element();
    let ns = Some("http://schemas.microsoft.com/appx/appinstaller/2017");
    fn attrs(node: roxmltree::Node<'_, '_>, keys: &[&str]) -> bool {
        node.attributes().len() == keys.len()
            && node
                .attributes()
                .all(|a| a.namespace().is_none() && keys.contains(&a.name()))
    }
    let children: Vec<_> = root.children().filter(|n| n.is_element()).collect();
    if root.tag_name().namespace() != ns
        || root.tag_name().name() != "AppInstaller"
        || !attrs(root, &["Version", "Uri"])
        || root.attribute("Uri") != Some(policy.descriptor.as_str())
        || children.len() != 2
        || children[0].tag_name().namespace() != ns
        || children[0].tag_name().name() != "MainPackage"
        || children[1].tag_name().namespace() != ns
        || children[1].tag_name().name() != "Dependencies"
        || children[1].attributes().len() != 0
        || !attrs(
            children[0],
            &[
                "Name",
                "Publisher",
                "Version",
                "ProcessorArchitecture",
                "Uri",
            ],
        )
    {
        return Err("Unexpected App Installer structure or update settings.".into());
    }
    let main = children[0];
    let dependencies: Vec<_> = children[1].children().filter(|n| n.is_element()).collect();
    if dependencies.len() != 1
        || dependencies[0].tag_name().namespace() != ns
        || dependencies[0].tag_name().name() != "Package"
        || !attrs(
            dependencies[0],
            &[
                "Name",
                "Publisher",
                "Version",
                "ProcessorArchitecture",
                "Uri",
            ],
        )
        || main.children().any(|n| n.is_element())
        || dependencies[0].children().any(|n| n.is_element())
        || doc
            .descendants()
            .any(|n| n.is_text() && n.text().is_some_and(|s| !s.trim().is_empty()))
    {
        return Err("Unexpected App Installer dependency structure.".into());
    }
    let runtime = dependencies[0];
    let v = version(
        main.attribute("Version")
            .ok_or("Missing native package version.")?,
    )?;
    let runtime_v = version(
        runtime
            .attribute("Version")
            .ok_or("Missing runtime version.")?,
    )?;
    let package = main.attribute("Uri").ok_or("Missing native package URI.")?;
    let uri = https(package)?;
    let approved = https(&policy.descriptor)?;
    if v <= source.version
        || runtime_v < [14, 0, 30704, 0]
        || root.attribute("Version") != main.attribute("Version")
        || main.attribute("Name") != Some(&policy.name)
        || main.attribute("Publisher") != Some(&policy.publisher)
        || main.attribute("ProcessorArchitecture") != Some("x64")
        || runtime.attribute("Name") != Some(&policy.runtime_name)
        || runtime.attribute("Publisher") != Some(&policy.runtime_publisher)
        || runtime.attribute("ProcessorArchitecture") != Some("x64")
        || runtime.attribute("Uri") != Some(&policy.runtime_uri)
        || uri.origin() != approved.origin()
        || uri.path().rsplit_once('/').map(|v| v.0) != approved.path().rsplit_once('/').map(|v| v.0)
        || !uri.path().ends_with(".msix")
        || uri == approved
    {
        return Err("App Installer target is not an approved newer package and runtime.".into());
    }
    Ok(Selection {
        version: v,
        package: package.into(),
        runtime_version: runtime_v,
    })
}

fn regular(path: &Path) -> Result<()> {
    let metadata =
        fs::symlink_metadata(path).map_err(|_| "Windows update storage is unavailable.")?;
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if metadata.file_attributes() & 0x400 != 0 {
            return Err("Windows update storage contains a reparse point.".into());
        }
    }
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err("Windows update media is not a regular file.".into());
    }
    Ok(())
}
fn directory(path: &Path) -> Result<()> {
    let metadata =
        fs::symlink_metadata(path).map_err(|_| "Windows update directory is unavailable.")?;
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if metadata.file_attributes() & 0x400 != 0 {
            return Err("Windows update directory is a reparse point.".into());
        }
    }
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err("Windows update storage is not a regular directory.".into());
    }
    Ok(())
}

struct Directory {
    file: File,
    path: PathBuf,
    identity: (u64, u64),
}
impl Directory {
    fn open(path: &Path) -> Result<Self> {
        directory(path)?;
        let mut options = OpenOptions::new();
        options.read(true);
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            options.custom_flags(0x02000000).share_mode(3); // directory handle, no delete sharing
        }
        let file = options
            .open(path)
            .map_err(|_| "Cannot retain the owned Windows download directory.")?;
        let identity = crate::file_safety::identity(&file)
            .map_err(|_| "Cannot bind Windows download storage identity.")?;
        let result = Self {
            file,
            path: path.into(),
            identity,
        };
        result.check()?;
        Ok(result)
    }
    fn check(&self) -> Result<()> {
        directory(&self.path)?;
        let mut options = OpenOptions::new();
        options.read(true);
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            options.custom_flags(0x02000000).share_mode(3);
        }
        let current = options
            .open(&self.path)
            .map_err(|_| "Windows download directory moved.")?;
        if crate::file_safety::identity(&current)
            .map_err(|_| "Cannot bind Windows download storage identity.")?
            != self.identity
            || crate::file_safety::identity(&self.file)
                .map_err(|_| "Cannot bind Windows download storage identity.")?
                != self.identity
        {
            return Err(
                "Windows download directory changed; retained files were not reused.".into(),
            );
        }
        Ok(())
    }
}

struct Held {
    file: File,
    path: PathBuf,
    media: Media,
    identity: (u64, u64),
}
impl Held {
    fn open(path: &Path, limit: u64, control: &Control) -> Result<Self> {
        regular(path)?;
        let mut options = OpenOptions::new();
        options.read(true);
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            options.share_mode(1); // read sharing only: deny writes and deletion while verified.
        }
        let mut file = options
            .open(path)
            .map_err(|_| "Cannot protect downloaded Windows media.")?;
        let identity = crate::file_safety::identity(&file)
            .map_err(|_| "Cannot bind Windows download file identity.")?;
        let media = fingerprint(&mut file, limit, control)?;
        let result = Self {
            file,
            path: path.into(),
            media,
            identity,
        };
        result.check(control)?;
        Ok(result)
    }
    fn check(&self, control: &Control) -> Result<()> {
        control.check()?;
        regular(&self.path)?;
        let mut reopened = File::open(&self.path).map_err(|_| "Downloaded Windows media moved.")?;
        if crate::file_safety::identity(&reopened)
            .map_err(|_| "Cannot bind Windows download file identity.")?
            != self.identity
            || crate::file_safety::identity(&self.file)
                .map_err(|_| "Cannot bind Windows download file identity.")?
                != self.identity
            || fingerprint(&mut reopened, PACKAGE, control)? != self.media
        {
            return Err("Downloaded Windows media changed; nothing was installed.".into());
        }
        Ok(())
    }
}
fn fingerprint(file: &mut File, limit: u64, control: &Control) -> Result<Media> {
    use std::io::{Seek, SeekFrom};
    file.seek(SeekFrom::Start(0))
        .map_err(|_| "Cannot read downloaded media.")?;
    let mut digest = ring::digest::Context::new(&ring::digest::SHA256);
    let mut bytes = 0u64;
    let mut block = [0; 64 * 1024];
    loop {
        control.check()?;
        let n = file
            .read(&mut block)
            .map_err(|_| "Cannot read downloaded media.")?;
        if n == 0 {
            break;
        }
        bytes = bytes
            .checked_add(n as u64)
            .filter(|n| *n <= limit)
            .ok_or("Windows media exceeds its byte budget.")?;
        digest.update(&block[..n]);
    }
    if bytes == 0 {
        return Err("Downloaded Windows media is empty.".into());
    }
    Ok(Media {
        bytes,
        sha256: hex(digest.finish().as_ref()),
    })
}

fn download(
    transport: &dyn Transport,
    uri: &str,
    path: &Path,
    limit: u64,
    control: &Control,
    phase: &'static str,
) -> Result<Held> {
    https(uri)?;
    control.phase(phase, 0)?;
    let mut response = transport.get(uri)?;
    if response.length == 0 || response.length > limit {
        return Err("Windows download exceeds its byte budget.".into());
    }
    let mut out = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(path)
        .map_err(|_| "Cannot create owned Windows download.")?;
    let identity = crate::file_safety::identity(&out)
        .map_err(|_| "Cannot bind Windows download file identity.")?;
    let mut digest = ring::digest::Context::new(&ring::digest::SHA256);
    let mut total = 0;
    let mut block = [0; 64 * 1024];
    loop {
        control.check()?;
        let n = response
            .reader
            .read(&mut block)
            .map_err(|_| "Windows download was interrupted; try again later.")?;
        if n == 0 {
            break;
        }
        total += n as u64;
        if total > response.length || total > limit {
            return Err("Windows download byte length changed.".into());
        }
        out.write_all(&block[..n])
            .map_err(|_| "Windows download storage failed; nothing was installed.")?;
        digest.update(&block[..n]);
        control.phase(phase, total)?;
    }
    if total != response.length {
        return Err("Windows download ended before its declared byte length.".into());
    }
    out.sync_all()
        .map_err(|_| "Cannot retain Windows download bytes.")?;
    drop(out);
    let held = Held::open(path, limit, control)?;
    if held.identity != identity
        || held.media
            != (Media {
                bytes: total,
                sha256: hex(digest.finish().as_ref()),
            })
    {
        return Err("Windows download bytes changed before verification.".into());
    }
    Ok(held)
}

fn small(file: &Held, control: &Control) -> Result<Vec<u8>> {
    use std::io::{Seek, SeekFrom};
    if file.media.bytes > SMALL {
        return Err("Windows update metadata exceeds its bound.".into());
    }
    let mut input = file
        .file
        .try_clone()
        .map_err(|_| "Cannot read Windows update metadata.")?;
    input
        .seek(SeekFrom::Start(0))
        .map_err(|_| "Cannot read Windows update metadata.")?;
    let mut bytes = Vec::with_capacity(file.media.bytes as usize);
    input
        .take(SMALL + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "Cannot read Windows update metadata.")?;
    file.check(control)?;
    if bytes.len() as u64 != file.media.bytes || hash(&bytes) != file.media.sha256 {
        return Err("Windows update metadata changed.".into());
    }
    Ok(bytes)
}

struct RetainedSelection {
    owned: Directory,
    journal: Held,
    descriptor: Held,
    package: Held,
    runtime: Held,
    prepared: Prepared,
    target: Option<Target>,
}
impl RetainedSelection {
    fn check_media(&self, control: &Control) -> Result<()> {
        for file in [
            &self.journal,
            &self.descriptor,
            &self.package,
            &self.runtime,
        ] {
            file.check(control)?;
        }
        self.owned.check()?;
        Ok(())
    }
    fn check(&self, verifier: &dyn Verifier, control: &Control) -> Result<()> {
        self.check_media(control)?;
        if verifier.source(&self.prepared.policy)? != self.prepared.source {
            return Err("Installed Windows source changed; staging evidence is retained.".into());
        }
        Ok(())
    }
    fn check_target(
        &self,
        running: &RunningTarget,
        policy: &Policy,
        verifier: &dyn Verifier,
        control: &Control,
    ) -> Result<()> {
        self.check_media(control)?;
        let target = self.target.as_ref().ok_or(STAGE_HOLD)?;
        running.check_files(target, control)?;
        if !target.matches(&running.current, policy) || verifier.source(policy)? != running.current
        {
            return Err(STAGE_HOLD.into());
        }
        Ok(())
    }
}
// This borrowed proof keeps the actual signed archive, retained media/parent
// handles and actual running-file proof alive together. It is not serializable
// and grants no application, registration, health or rollback authority.
struct CurrentArchive<'a> {
    retained: &'a RetainedSelection,
    running: RunningTarget,
}
impl CurrentArchive<'_> {
    fn check(&self, policy: &Policy, verifier: &dyn Verifier, control: &Control) -> Result<()> {
        self.retained
            .check_target(&self.running, policy, verifier, control)
    }
}
fn current_archive<'a>(
    retained: &'a RetainedSelection,
    current: &Source,
    policy: &Policy,
    verifier: &dyn Verifier,
    control: &Control,
) -> Result<CurrentArchive<'a>> {
    let target = retained.target.as_ref().ok_or(STAGE_HOLD)?;
    if !target.matches(current, policy) || target.version != retained.prepared.selection.version {
        return Err(STAGE_HOLD.into());
    }
    let running = verifier.running_target(policy, target, control)?;
    if &running.current != current {
        return Err(STAGE_HOLD.into());
    }
    let proof = CurrentArchive { retained, running };
    proof.check(policy, verifier, control)?;
    Ok(proof)
}
fn reconcile(
    root: &Path,
    policy: &Policy,
    verifier: &dyn Verifier,
    control: &Control,
) -> Result<Prepared> {
    Ok(retain_selection(root, policy, verifier, control)?.prepared)
}

fn retain_selection(
    root: &Path,
    policy: &Policy,
    verifier: &dyn Verifier,
    control: &Control,
) -> Result<RetainedSelection> {
    no_stage_evidence(root)?;
    let retained = authenticate_retained_media(root, policy, verifier, control)?;
    no_stage_evidence(root)?;
    Ok(retained)
}
fn authenticate_retained_media(
    root: &Path,
    policy: &Policy,
    verifier: &dyn Verifier,
    control: &Control,
) -> Result<RetainedSelection> {
    authenticate_original(
        read_prepared(root, policy, control)?,
        policy,
        verifier,
        control,
    )
}
fn read_prepared(
    root: &Path,
    policy: &Policy,
    control: &Control,
) -> Result<(Directory, Held, Prepared)> {
    let owned = Directory::open(root)?;
    let journal = Held::open(&root.join("prepared.json"), SMALL, control)?;
    let prepared: Prepared = serde_json::from_slice(&small(&journal, control)?)
        .map_err(|_| "Pending Windows download metadata is invalid; original retained.")?;
    policy.validate()?;
    if prepared.schema != 1 || &prepared.policy != policy {
        return Err(
            "Installed source or approved Windows policy changed; pending media was not reused."
                .into(),
        );
    }
    Ok((owned, journal, prepared))
}
fn authenticate_original(
    input: (Directory, Held, Prepared),
    policy: &Policy,
    verifier: &dyn Verifier,
    control: &Control,
) -> Result<RetainedSelection> {
    if input.2.source != verifier.source(policy)? {
        return Err(
            "Installed source or approved Windows policy changed; pending media was not reused."
                .into(),
        );
    }
    let retained = authenticate_media(input, policy, verifier, None, control)?;
    retained.check(verifier, control)?;
    control.check()?;
    Ok(retained)
}
fn authenticate_media(
    input: (Directory, Held, Prepared),
    policy: &Policy,
    verifier: &dyn Verifier,
    current: Option<&Source>,
    control: &Control,
) -> Result<RetainedSelection> {
    let (owned, journal, prepared) = input;
    let root = &owned.path;
    let descriptor_file = Held::open(&root.join("selected.appinstaller"), SMALL, control)?;
    if descriptor_file.media != prepared.descriptor
        || descriptor(&small(&descriptor_file, control)?, policy, &prepared.source)?
            != prepared.selection
    {
        return Err("Pending Windows descriptor changed.".into());
    }
    let package = Held::open(&root.join("main.msix"), PACKAGE, control)?;
    let runtime = Held::open(&root.join("runtime.appx"), PACKAGE, control)?;
    if package.media != prepared.package || runtime.media != prepared.runtime {
        return Err("Pending Windows media changed.".into());
    }
    control.phase("Revalidating retained signed Windows media", 0)?;
    let target = if let Some(current) = current {
        Some(verifier.current_media(&package, policy, current, control)?)
    } else {
        verifier.media(&package, policy, &prepared.selection, false, control)?
    };
    verifier.media(&runtime, policy, &prepared.selection, true, control)?;
    for file in [&journal, &descriptor_file, &package, &runtime] {
        file.check(control)?;
    }
    owned.check()?;
    control.check()?;
    Ok(RetainedSelection {
        owned,
        journal,
        descriptor: descriptor_file,
        package,
        runtime,
        prepared,
        target,
    })
}

fn prepare(
    root: &Path,
    policy: &Policy,
    transport: &dyn Transport,
    verifier: &dyn Verifier,
    control: &Control,
) -> Result<Prepared> {
    policy.validate()?;
    let owned = Directory::open(root)?;
    let marker = root.join("prepared.json");
    let versions = root.join(".peerbrush-versions");
    if fs::symlink_metadata(&versions).is_ok() {
        directory(&versions)?;
    }
    let lease = crate::file_safety::Lease::acquire(&marker)
        .map_err(|_| "Cannot acquire the owned Windows transaction lease.")?;
    directory(&versions)?;
    if marker
        .try_exists()
        .map_err(|_| "Cannot inspect retained Windows download.")?
    {
        return reconcile(root, policy, verifier, control);
    }
    // Never truncate/guess ownership of remnants. A new transaction requires a
    // fresh directory; only its validated Prepared record can be reused.
    for entry in fs::read_dir(root).map_err(|_| "Cannot inspect Windows download directory.")? {
        if entry
            .map_err(|_| "Cannot inspect Windows download directory.")?
            .file_name()
            != ".peerbrush-versions"
        {
            return Err(
                "An interrupted Windows download is retained; a fresh owned attempt is required."
                    .into(),
            );
        }
    }
    let source = verifier.source(policy)?;
    let descriptor_file = download(
        transport,
        &policy.descriptor,
        &root.join("selected.appinstaller"),
        SMALL,
        control,
        "Reading approved Windows descriptor",
    )?;
    let selection = descriptor(&small(&descriptor_file, control)?, policy, &source)?;
    let package = download(
        transport,
        &selection.package,
        &root.join("main.msix"),
        PACKAGE,
        control,
        "Downloading signed Windows package",
    )?;
    let runtime = download(
        transport,
        &policy.runtime_uri,
        &root.join("runtime.appx"),
        PACKAGE,
        control,
        "Downloading signed Windows runtime",
    )?;
    control.phase("Verifying Windows signatures and native payloads", 0)?;
    verifier.media(&package, policy, &selection, false, control)?;
    verifier.media(&runtime, policy, &selection, true, control)?;
    let prepared = Prepared {
        schema: 1,
        source: source.clone(),
        policy: policy.clone(),
        selection,
        descriptor: descriptor_file.media.clone(),
        package: package.media.clone(),
        runtime: runtime.media.clone(),
    };
    for file in [&descriptor_file, &package, &runtime] {
        file.check(control)?;
    }
    if verifier.source(policy)? != source {
        return Err("Installed Windows source changed during download.".into());
    }
    owned.check()?;
    let bytes =
        serde_json::to_vec(&prepared).map_err(|_| "Cannot retain prepared Windows metadata.")?;
    if bytes.len() as u64 > SMALL {
        return Err("Prepared Windows metadata exceeds its bound.".into());
    }
    crate::server::atomic_write_checked(&marker, &bytes, |_| {
        control.check()?;
        owned.check()?;
        lease
            .validate()
            .map_err(|_| "Windows transaction destination changed.".into())
    })
    .map_err(|_| {
        "Cannot publish verified Windows transaction metadata; retained bytes need revalidation."
    })?;
    let saved = Held::open(&marker, SMALL, control)?;
    if small(&saved, control)? != bytes {
        return Err("Prepared Windows metadata did not read back exactly.".into());
    }
    owned.check()?;
    // The absence guarded by lease is now intentionally replaced. Authenticate
    // the published marker's held identity/current bytes, not its old absence.
    saved.check(control)?;
    control.check()?;
    Ok(prepared)
}

fn no_stage_evidence(root: &Path) -> Result<()> {
    for marker in ["stage-intent.json", "stage-outcome.json"] {
        match fs::symlink_metadata(root.join(marker)) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            _ => return Err(STAGE_HOLD.into()),
        }
    }
    Ok(())
}

fn attempts(root: &Path) -> Result<(Vec<PathBuf>, Option<PathBuf>)> {
    let result = attempt_inventory(root)?;
    for path in &result.0 {
        no_stage_evidence(path)?;
    }
    Ok(result)
}
fn attempt_inventory(root: &Path) -> Result<(Vec<PathBuf>, Option<PathBuf>)> {
    let mut attempts = Vec::new();
    let mut prepared = None;
    for item in fs::read_dir(root).map_err(|_| "Cannot inspect Windows acquisition state.")? {
        let item = item.map_err(|_| "Cannot inspect Windows acquisition state.")?;
        if item.file_name() == ".peerbrush-versions" {
            continue;
        }
        let name = item
            .file_name()
            .into_string()
            .map_err(|_| "Unrecognized Windows acquisition state; files retained.")?;
        let id = name
            .strip_prefix("attempt-")
            .ok_or("Unrecognized Windows acquisition state; files retained.")?;
        let parsed = uuid::Uuid::parse_str(id)
            .map_err(|_| "Unrecognized Windows acquisition directory; files retained.")?;
        if parsed.to_string() != id {
            return Err("Unrecognized Windows acquisition directory; files retained.".into());
        }
        directory(&item.path())?;
        match fs::symlink_metadata(item.path().join("prepared.json")) {
            Ok(_) => {
                if prepared.replace(item.path()).is_some() {
                    return Err(
                        "Multiple retained Windows selections are ambiguous; none was accepted."
                            .into(),
                    );
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => {
                return Err("Cannot inspect retained Windows selection; files retained.".into())
            }
        }
        attempts.push(item.path());
        if attempts.len() > 3 {
            return Err("Windows acquisition state exceeds its retained-attempt budget.".into());
        }
    }
    Ok((attempts, prepared))
}

fn acquire(
    root: &Path,
    policy: &Policy,
    transport: &dyn Transport,
    verifier: &dyn Verifier,
    control: &Control,
) -> Result<Prepared> {
    policy.validate()?;
    let owned = Directory::open(root)?;
    let versions = root.join(".peerbrush-versions");
    if fs::symlink_metadata(&versions).is_ok() {
        directory(&versions)?;
    }
    // One existing lease guards selection across all attempts, including a
    // canceled native verification whose owning Controller has been dropped.
    let lease = crate::file_safety::Lease::acquire(&root.join("acquisition.json"))
        .map_err(|_| "Another Windows acquisition owns this cache or its lease is unavailable.")?;
    directory(&versions)?;
    let (attempts, prepared) = attempts(root)?;
    if let Some(path) = prepared {
        let result = reconcile(&path, policy, verifier, control)?;
        owned.check()?;
        lease
            .validate()
            .map_err(|_| "Windows acquisition destination changed.")?;
        return Ok(result);
    }
    // shortcut: three retained download-only attempts, add reviewed ownership-safe
    // cache retirement before admitting more retries. Never truncate partial bytes.
    if attempts.len() == 3 {
        return Err("Three Windows download attempts are retained; cache retirement is not implemented. Nothing was deleted or installed.".into());
    }
    // Check the executing/current-user source before creating another attempt.
    // A partial transaction never authenticates or authorizes a retry's bytes.
    verifier.source(policy)?;
    control.check()?;
    owned.check()?;
    lease
        .validate()
        .map_err(|_| "Windows acquisition destination changed.")?;
    let attempt = root.join(format!("attempt-{}", crate::engine::id()));
    fs::create_dir(&attempt)
        .map_err(|_| "Cannot create a fresh owned Windows acquisition attempt.")?;
    let result = prepare(&attempt, policy, transport, verifier, control)?;
    owned.check()?;
    lease
        .validate()
        .map_err(|_| "Windows acquisition destination changed.")?;
    Ok(result)
}

const STAGE_HOLD: &str =
    "Windows staging state is retained; registration and restart remain unavailable.";
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StageTerminal {
    hresult: Option<i32>,
    registered: Option<bool>,
    canceled: bool,
    invalid_progress: bool,
}
enum StageObservation {
    Pending,
    Uncertain,
    Terminal(StageTerminal),
}
// Progress and terminal delivery share a linearization point. Never hold this
// lock across native Status/GetResults, which can invoke the progress handler.
struct StageProgress {
    state: Mutex<StageProgressState>,
    view: Arc<Mutex<View>>,
}
#[derive(Default)]
struct StageProgressState {
    invalid: bool,
    finished: bool,
}
impl StageProgress {
    fn new(view: Arc<Mutex<View>>) -> Self {
        Self {
            state: Mutex::new(StageProgressState::default()),
            view,
        }
    }
    fn percentage(&self, percentage: u32) {
        let mut state = self.state.lock().unwrap_or_else(|p| p.into_inner());
        if state.finished || state.invalid {
            return;
        }
        if percentage > 100 {
            state.invalid = true;
            return;
        }
        let mut view = self.view.lock().unwrap_or_else(|p| p.into_inner());
        view.phase = "Staging signed Windows package with Windows";
        view.percentage = Some(percentage);
    }
    fn invalidate(&self) {
        let mut state = self.state.lock().unwrap_or_else(|p| p.into_inner());
        if !state.finished {
            state.invalid = true;
        }
    }
    fn poll(&self, read: impl FnOnce() -> StageObservation) -> StageObservation {
        let observed = read();
        let mut state = self.state.lock().unwrap_or_else(|p| p.into_inner());
        match observed {
            StageObservation::Terminal(mut terminal) => {
                state.finished = true;
                terminal.invalid_progress |= state.invalid;
                StageObservation::Terminal(terminal)
            }
            _ if state.invalid => StageObservation::Uncertain,
            other => other,
        }
    }
}
trait StageOperation {
    fn poll(&mut self) -> StageObservation;
    fn cancel(&mut self);
}
trait Stager {
    fn submit(
        &self,
        selection: &RetainedSelection,
        view: Arc<Mutex<View>>,
    ) -> Result<Box<dyn StageOperation>>;
}
#[derive(Serialize)]
struct StageIntent<'a> {
    schema: u32,
    prepared_sha256: &'a str,
    prepared: &'a Prepared,
}
#[derive(Serialize)]
struct StageOutcome<'a> {
    schema: u32,
    intent_sha256: &'a str,
    terminal: &'a StageTerminal,
    rejected: bool,
}
struct Staged {
    version: [u16; 4],
    accepted: bool,
}
fn stage_record(path: &Path, value: &impl Serialize, control: &Control) -> Result<Held> {
    let lease = crate::file_safety::Lease::acquire(path)?;
    if lease.expected.is_some() {
        return Err(STAGE_HOLD.into());
    }
    let bytes = serde_json::to_vec(value).map_err(|_| STAGE_HOLD)?;
    if bytes.len() as u64 > SMALL {
        return Err(STAGE_HOLD.into());
    }
    crate::server::atomic_write_checked(path, &bytes, |_| lease.validate())?;
    let saved = Held::open(path, SMALL, control)?;
    if small(&saved, control)? != bytes {
        return Err(STAGE_HOLD.into());
    }
    Ok(saved)
}
fn stage(
    root: &Path,
    policy: &Policy,
    verifier: &dyn Verifier,
    stager: &dyn Stager,
    control: &Control,
) -> Result<Staged> {
    policy.validate()?;
    control.check()?;
    let owned = Directory::open(root)?;
    let versions = root.join(".peerbrush-versions");
    if fs::symlink_metadata(&versions).is_ok() {
        directory(&versions)?;
    }
    let acquisition = crate::file_safety::Lease::acquire(&root.join("acquisition.json"))?;
    directory(&versions)?;
    let (_, selected) = attempts(root)?;
    let selected = selected.ok_or("Prepare signed Windows media before staging.")?;
    let transaction_versions = selected.join(".peerbrush-versions");
    directory(&transaction_versions)?;
    let transaction = crate::file_safety::Lease::acquire(&selected.join("prepared.json"))?;
    directory(&transaction_versions)?;
    let retained = retain_selection(&selected, policy, verifier, control)?;
    retained.check(verifier, control)?;
    owned.check()?;
    acquisition.validate()?;
    transaction.validate()?;
    let intent = stage_record(
        &selected.join("stage-intent.json"),
        &StageIntent {
            schema: 1,
            prepared_sha256: &retained.journal.media.sha256,
            prepared: &retained.prepared,
        },
        control,
    )?;
    // A durable intent precedes the provider boundary. No automatic retirement,
    // cleanup or retry is safe after submission, including after process loss.
    control.check()?;
    retained.check(verifier, control)?;
    let mut operation = stager.submit(&retained, control.view.clone())?;
    let mut rejected = false;
    let mut cancellation_sent = false;
    let terminal = loop {
        if control.cancelled.load(Ordering::Acquire) || Instant::now() >= control.deadline {
            rejected = true;
        }
        if rejected && !cancellation_sent {
            operation.cancel();
            cancellation_sent = true;
        }
        match operation.poll() {
            StageObservation::Pending => {}
            StageObservation::Uncertain => {
                rejected = true;
            }
            StageObservation::Terminal(value) => {
                rejected |= value.invalid_progress;
                break value;
            }
        }
        if rejected {
            let mut view = control.view.lock().unwrap_or_else(|p| p.into_inner());
            view.phase = "Waiting for the owned Windows staging operation";
            view.message = STAGE_HOLD.into();
        }
        std::thread::sleep(Duration::from_millis(25));
    };
    rejected |= control.cancelled.load(Ordering::Acquire) || Instant::now() >= control.deadline;
    // Late terminal observations are retained even after Cancel/timeout. A new
    // independent bounded readback is not canceled by the old UI request.
    let readback = Control {
        cancelled: Arc::new(AtomicBool::new(false)),
        deadline: Instant::now() + TIMEOUT,
        view: control.view.clone(),
    };
    let _outcome = stage_record(
        &selected.join("stage-outcome.json"),
        &StageOutcome {
            schema: 1,
            intent_sha256: &intent.media.sha256,
            terminal: &terminal,
            rejected,
        },
        &readback,
    )?;
    retained.check(verifier, &readback)?;
    intent.check(&readback)?;
    owned.check()?;
    acquisition.validate()?;
    transaction.validate()?;
    // shortcut: submitted staging attempts are never automatically retried or
    // retired; add reviewed outcome/retirement before selecting another update.
    Ok(Staged {
        version: retained.prepared.selection.version,
        accepted: !rejected
            && !terminal.canceled
            && terminal.hresult == Some(0)
            && terminal.registered == Some(false),
    })
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RecordedIntent {
    schema: u32,
    prepared_sha256: String,
    prepared: Prepared,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RecordedOutcome {
    schema: u32,
    intent_sha256: String,
    terminal: StageTerminal,
    rejected: bool,
}
enum Reconciled {
    Empty,
    Prepared(Prepared),
    RecordedStaging([u16; 4]),
    SelectedTargetRunning([u16; 4]),
}
// Retained observations grant no installation, OS-stage, cancellation or cleanup
// authority. A native current-user query is separate from this record's target.
fn inspect_retained(
    root: &Path,
    policy: &Policy,
    verifier: &dyn Verifier,
    control: &Control,
) -> Result<Reconciled> {
    policy.validate()?;
    control.check()?;
    let current = verifier.source(policy)?;
    control.view.lock().unwrap().installed_version = Some(current.version);
    match fs::symlink_metadata(root) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Reconciled::Empty),
        Err(_) => return Err(STAGE_HOLD.into()),
        Ok(_) => {}
    }
    let owned = Directory::open(root)?;
    let versions = root.join(".peerbrush-versions");
    if fs::symlink_metadata(&versions).is_ok() {
        directory(&versions)?;
    }
    let acquisition = crate::file_safety::Lease::acquire(&root.join("acquisition.json"))?;
    directory(&versions)?;
    let (mut attempts, selected) = attempt_inventory(root)?;
    for path in &attempts {
        if selected.as_ref() != Some(path) {
            no_stage_evidence(path)?;
        }
    }
    let mut authenticated = None;
    let mut evidence = Vec::new();
    let mut transaction_owner = None;
    let result = if let Some(selected) = &selected {
        directory(&selected.join(".peerbrush-versions"))?;
        let transaction = crate::file_safety::Lease::acquire(&selected.join("prepared.json"))?;
        directory(&selected.join(".peerbrush-versions"))?;
        let input = read_prepared(selected, policy, control)?;
        let selected_running = current != input.2.source;
        let retained = if selected_running {
            authenticate_media(input, policy, verifier, Some(&current), control)?
        } else {
            authenticate_original(input, policy, verifier, control)?
        };
        let running = if selected_running {
            // Observation consumes the running member only; retained continues
            // owning the verified current archive through every final recheck.
            Some(current_archive(&retained, &current, policy, verifier, control)?.running)
        } else {
            None
        };
        let result = if no_stage_evidence(selected).is_ok() {
            let prepared = retained.prepared.clone();
            if let Some(running) = &running {
                retained.check_target(running, policy, verifier, control)?;
            } else {
                retained.check(verifier, control)?;
            }
            no_stage_evidence(selected)?;
            Reconciled::Prepared(prepared)
        } else {
            let intent_file = Held::open(&selected.join("stage-intent.json"), SMALL, control)?;
            let intent: RecordedIntent =
                serde_json::from_slice(&small(&intent_file, control)?).map_err(|_| STAGE_HOLD)?;
            if intent.schema != 1
                || intent.prepared_sha256 != retained.journal.media.sha256
                || intent.prepared != retained.prepared
            {
                return Err(STAGE_HOLD.into());
            }
            let outcome_file = Held::open(&selected.join("stage-outcome.json"), SMALL, control)?;
            let outcome: RecordedOutcome =
                serde_json::from_slice(&small(&outcome_file, control)?).map_err(|_| STAGE_HOLD)?;
            if outcome.schema != 1
                || outcome.intent_sha256 != intent_file.media.sha256
                || outcome.rejected
                || outcome.terminal.canceled
                || outcome.terminal.invalid_progress
                || outcome.terminal.hresult != Some(0)
                || outcome.terminal.registered != Some(false)
            {
                return Err(STAGE_HOLD.into());
            }
            if let Some(running) = &running {
                retained.check_target(running, policy, verifier, control)?;
            } else {
                retained.check(verifier, control)?;
            }
            intent_file.check(control)?;
            outcome_file.check(control)?;
            evidence.push(intent_file);
            evidence.push(outcome_file);
            Reconciled::RecordedStaging(retained.prepared.selection.version)
        };
        transaction.validate()?;
        transaction_owner = Some(transaction);
        let result = if let Some(running) = &running {
            Reconciled::SelectedTargetRunning(running.current.version)
        } else {
            result
        };
        authenticated = Some((retained, running));
        result
    } else {
        Reconciled::Empty
    };
    let (mut after, after_selected) = attempt_inventory(root)?;
    attempts.sort();
    after.sort();
    if attempts != after || selected != after_selected || verifier.source(policy)? != current {
        return Err(STAGE_HOLD.into());
    }
    if let Some((retained, running)) = &authenticated {
        if let Some(running) = running {
            retained.check_target(running, policy, verifier, control)?;
        } else {
            retained.check(verifier, control)?;
        }
    }
    for file in &evidence {
        file.check(control)?;
    }
    if let Some(transaction) = &transaction_owner {
        transaction.validate()?;
    }
    owned.check()?;
    acquisition.validate()?;
    control.check()?;
    Ok(result)
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum DeploymentState {
    Unchecked,
    Clear,
    Recorded,
    Unknown,
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum WorkKind {
    Download,
    Stage,
    Reconcile,
}
enum Completed {
    Download(Prepared),
    Stage(Staged),
    Reconciled(Reconciled),
}

struct Pending {
    receiver: mpsc::Receiver<Result<Completed>>,
    kind: WorkKind,
    cancel: Arc<AtomicBool>,
    view: Arc<Mutex<View>>,
    deadline: Instant,
}
#[cfg(test)]
type FakeWork = Box<dyn FnOnce(&Control) -> Result<Prepared> + Send>;
#[cfg(test)]
type FakeStage = Box<dyn FnOnce(&Control) -> Result<Staged> + Send>;
#[cfg(test)]
type FakeReconcile = Box<dyn FnOnce(&Control) -> Result<Reconciled> + Send>;
pub(crate) struct Controller {
    root: PathBuf,
    profile: Option<crate::settings::UpdateProfile>,
    pub view: View,
    active: Option<Pending>,
    deployment: DeploymentState,
    #[cfg(test)]
    next: Option<FakeWork>,
    #[cfg(test)]
    next_stage: Option<FakeStage>,
    #[cfg(test)]
    next_reconcile: Option<FakeReconcile>,
}
impl Controller {
    pub fn load(state: &Path) -> Self {
        Self {
            root: state.join("windows-prepared"),
            profile: None,
            view: View::default(),
            active: None,
            deployment: DeploymentState::Unchecked,
            #[cfg(test)]
            next: None,
            #[cfg(test)]
            next_stage: None,
            #[cfg(test)]
            next_reconcile: None,
        }
    }
    pub(crate) fn set_profile(&mut self, profile: Option<crate::settings::UpdateProfile>) {
        self.profile = profile;
    }
    fn staging_profile(&self) -> Result<crate::settings::UpdateProfile> {
        let profile = self
            .profile
            .clone()
            .ok_or("Native Windows staging requires this app's default profile lease.")?;
        if !self.root.is_absolute()
            || self.root != profile.locations.profile().join("windows-prepared")
        {
            return Err(
                "Native Windows staging cannot use an isolated or foreign profile root.".into(),
            );
        }
        Ok(profile)
    }
    pub(crate) fn deployment_blocker(&self) -> Option<&'static str> {
        match self.deployment {
            DeploymentState::Unknown => Some("Windows update state is unresolved. Update installation and restart are unavailable."),
            DeploymentState::Recorded => Some("A prior staging result is retained. Update installation and restart are unavailable."),
            _ => None,
        }
    }
    pub(crate) fn startup(&mut self) {
        if self.deployment != DeploymentState::Unchecked || self.active.is_some() {
            return;
        }
        #[cfg(test)]
        if self.next_reconcile.is_some() {
            if let Err(error) = self.start_reconciliation() {
                self.view.message = error;
            }
            return;
        }
        if ENABLED && configured().is_some() && self.staging_profile().is_ok() {
            if let Err(error) = self.start_reconciliation() {
                self.view.message = error;
            }
        }
    }
    pub fn start_reconciliation(&mut self) -> Result<()> {
        if self.active.is_some() {
            return Err(STAGE_HOLD.into());
        }
        #[cfg(test)]
        if let Some(work) = self.next_reconcile.take() {
            self.deployment = DeploymentState::Unknown;
            return self.launch_task(WorkKind::Reconcile, move |c| {
                work(c).map(Completed::Reconciled)
            });
        }
        if !ENABLED {
            return Err(DISABLED.into());
        }
        #[cfg(windows)]
        {
            let policy = configured().ok_or(DISABLED)?;
            let profile = self.staging_profile()?;
            let root = self.root.clone();
            self.deployment = DeploymentState::Unknown;
            self.launch_task(WorkKind::Reconcile, move |control| {
                let _profile = profile;
                _profile.validate()?;
                native::session(|| inspect_retained(&root, &policy, &native::Verifier, control))
                    .map(Completed::Reconciled)
            })
        }
        #[cfg(not(windows))]
        {
            Err(DISABLED.into())
        }
    }
    pub fn allowed(&self) -> bool {
        #[cfg(test)]
        if self.next.is_some() || self.next_stage.is_some() || self.next_reconcile.is_some() {
            return true;
        }
        ENABLED && configured().is_some()
    }
    pub fn start(&mut self) -> Result<()> {
        if matches!(
            self.deployment,
            DeploymentState::Unknown | DeploymentState::Recorded
        ) {
            return Err(STAGE_HOLD.into());
        }
        #[cfg(test)]
        if let Some(work) = self.next.take() {
            return self.launch(work);
        }
        if !self.allowed() {
            return Err(DISABLED.into());
        }
        if self.active.is_some() {
            return Err("A Windows download is still finishing or being canceled.".into());
        }
        #[cfg(windows)]
        {
            let policy = configured().ok_or(DISABLED)?;
            if !self.root.is_absolute() {
                return Err(
                    "Choose an absolute isolated Windows state directory before preparation."
                        .into(),
                );
            }
            let parent = self
                .root
                .parent()
                .ok_or("Missing private Windows state directory.")?;
            directory(parent)?;
            match fs::create_dir(&self.root) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => directory(&self.root)?,
                Err(_) => return Err("Cannot create Windows download directory.".into()),
            }
            let root = self.root.clone();
            self.launch(move |control| {
                if !ENABLED {
                    return Err(DISABLED.into());
                }
                native::session(|| {
                    acquire(&root, &policy, &Http::new(), &native::Verifier, control)
                })
            })
        }
        #[cfg(not(windows))]
        {
            Err(DISABLED.into())
        }
    }
    fn staging_allowed(&self) -> bool {
        #[cfg(test)]
        if self.next_stage.is_some() {
            return true;
        }
        self.allowed()
            && self.staging_profile().is_ok()
            && !matches!(
                self.deployment,
                DeploymentState::Unknown | DeploymentState::Recorded
            )
    }
    pub fn start_staging(&mut self) -> Result<()> {
        if matches!(
            self.deployment,
            DeploymentState::Unknown | DeploymentState::Recorded
        ) {
            return Err(STAGE_HOLD.into());
        }
        #[cfg(test)]
        if let Some(work) = self.next_stage.take() {
            return self.launch_task(WorkKind::Stage, move |control| {
                work(control).map(Completed::Stage)
            });
        }
        if !self.staging_allowed() {
            return Err(DISABLED.into());
        }
        #[cfg(windows)]
        {
            let policy = configured().ok_or(DISABLED)?;
            let profile = self.staging_profile()?;
            let root = self.root.clone();
            self.launch_task(WorkKind::Stage, move |control| {
                if !ENABLED {
                    return Err(DISABLED.into());
                }
                let _profile = profile;
                _profile.validate()?;
                native::session(|| {
                    stage(&root, &policy, &native::Verifier, &native::Stager, control)
                })
                .map(Completed::Stage)
            })
        }
        #[cfg(not(windows))]
        {
            Err(DISABLED.into())
        }
    }
    fn launch(
        &mut self,
        work: impl FnOnce(&Control) -> Result<Prepared> + Send + 'static,
    ) -> Result<()> {
        self.launch_task(WorkKind::Download, move |control| {
            work(control).map(Completed::Download)
        })
    }
    fn launch_task(
        &mut self,
        kind: WorkKind,
        work: impl FnOnce(&Control) -> Result<Completed> + Send + 'static,
    ) -> Result<()> {
        if self.active.is_some() {
            return Err("A Windows download is already retained.".into());
        }
        let (tx, rx) = mpsc::sync_channel(1);
        let cancel = Arc::new(AtomicBool::new(false));
        let view = Arc::new(Mutex::new(View {
            busy: true,
            phase: match kind {
                WorkKind::Stage => "Starting native Windows staging",
                WorkKind::Reconcile => "Verifying retained Windows staging state",
                WorkKind::Download => "Starting signed Windows download",
            },
            ..View::default()
        }));
        let deadline = Instant::now() + TIMEOUT;
        let control = Control {
            cancelled: cancel.clone(),
            deadline,
            view: view.clone(),
        };
        std::thread::Builder::new().name(if kind != WorkKind::Download { "peerbrush-windows-stage" } else { "peerbrush-windows-download" }.into()).spawn(move || {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| work(&control)))
                .unwrap_or_else(|_| Err("Windows preparation stopped unexpectedly; retained media needs revalidation.".into()));
            let _ = tx.send(result);
        }).map_err(|_| "Could not start signed Windows download.")?;
        self.view = view.lock().unwrap().clone();
        if kind != WorkKind::Download {
            self.deployment = DeploymentState::Unknown;
        }
        self.active = Some(Pending {
            receiver: rx,
            kind,
            cancel,
            view,
            deadline,
        });
        Ok(())
    }
    pub fn tick(&mut self) {
        let Some(pending) = &self.active else {
            return;
        };
        self.view = pending.view.lock().unwrap().clone();
        let timeout = Instant::now() >= pending.deadline;
        if timeout {
            pending.cancel.store(true, Ordering::Release);
        }
        if pending.cancel.load(Ordering::Acquire) {
            self.view.message = if pending.kind != WorkKind::Download {
                STAGE_HOLD
            } else if timeout {
                "Windows preparation exceeded its deadline; waiting for its owned native operation."
            } else {
                "Canceling signed Windows preparation; waiting for its owned operation."
            }
            .into();
        }
        let result = match pending.receiver.try_recv() {
            Ok(result) => Some(result),
            Err(mpsc::TryRecvError::Disconnected) => Some(Err(
                "Windows preparation ended unexpectedly; retained media needs revalidation.".into(),
            )),
            Err(mpsc::TryRecvError::Empty) => None,
        };
        if let Some(result) = result {
            let canceled = pending.cancel.load(Ordering::Acquire);
            let kind = pending.kind;
            self.active = None;
            self.view.busy = false;
            self.view.version = None;
            self.view.staged_version = None;
            self.view.recorded_staging = false;
            self.view.observed_target = None;
            self.view.message = if canceled && kind != WorkKind::Download {
                STAGE_HOLD.into()
            } else if canceled {
                "Windows preparation was canceled. Retained bytes must be revalidated before reuse; nothing was installed.".into()
            } else {
                match result {
                    Ok(Completed::Reconciled(Reconciled::Empty)) => {
                        self.deployment = DeploymentState::Clear;
                        "No retained staging record was found. Installation remains unavailable."
                            .into()
                    }
                    Ok(Completed::Reconciled(Reconciled::Prepared(prepared))) => {
                        self.deployment = DeploymentState::Clear;
                        self.view.version = Some(prepared.selection.version);
                        "Retained signed Windows media revalidated. Installation and restart remain unavailable.".into()
                    }
                    Ok(Completed::Reconciled(Reconciled::RecordedStaging(version))) => {
                        self.deployment = DeploymentState::Recorded;
                        self.view.staged_version = Some(version);
                        self.view.recorded_staging = true;
                        "Prior staging result verified. Installation is unverified; retries remain unavailable.".into()
                    }
                    Ok(Completed::Reconciled(Reconciled::SelectedTargetRunning(version))) => {
                        self.deployment = DeploymentState::Unknown;
                        self.view.observed_target = Some(version);
                        "The selected signed target is running. Startup health, recovery and installation acceptance remain unverified; retained records were not retired.".into()
                    }
                    Ok(Completed::Stage(staged)) if staged.accepted => {
                        self.deployment = DeploymentState::Recorded;
                        self.view.staged_version = Some(staged.version);
                        "Signed Windows package staged. Registration and restart remain unavailable.".into()
                    }
                    Ok(Completed::Download(prepared)) => {
                        self.view.version = Some(prepared.selection.version);
                        "Signed Windows media prepared locally. Installation and restart remain unavailable.".into()
                    }
                    _ if kind != WorkKind::Download => STAGE_HOLD.into(),
                    Err(error) => error,
                    _ => STAGE_HOLD.into(),
                }
            };
        }
    }
    pub fn later(&mut self) {
        // Prepared is retained. An unfinished operation is canceled but remains
        // owned/quarantined until terminal; there is never a replacement worker.
        if let Some(pending) = &self.active {
            pending.cancel.store(true, Ordering::Release);
        }
    }
    pub fn show(&mut self, ui: &mut eframe::egui::Ui) {
        ui.label("Signed Windows package preparation");
        if !self.allowed() {
            ui.label(DISABLED);
        }
        if ui
            .add_enabled(
                self.allowed()
                    && !self.view.busy
                    && !matches!(
                        self.deployment,
                        DeploymentState::Unknown | DeploymentState::Recorded
                    ),
                eframe::egui::Button::new("Prepare signed Windows update"),
            )
            .clicked()
        {
            if let Err(error) = self.start() {
                self.view.message = error;
            }
        }
        if ui
            .add_enabled(
                self.staging_allowed() && !self.view.busy && self.view.version.is_some(),
                eframe::egui::Button::new("Stage verified Windows package"),
            )
            .clicked()
        {
            if let Err(error) = self.start_staging() {
                self.view.message = error;
            }
        }
        if self.deployment != DeploymentState::Unchecked
            && ui
                .add_enabled(
                    self.allowed() && !self.view.busy,
                    eframe::egui::Button::new("Recheck retained Windows state"),
                )
                .clicked()
        {
            if let Err(error) = self.start_reconciliation() {
                self.view.message = error;
            }
        }
        if self.view.busy {
            ui.spinner();
            ui.label(format!(
                "{} · {} KiB",
                self.view.phase,
                self.view.bytes / 1024
            ));
            if let Some(percentage) = self.view.percentage {
                ui.label(format!("Native staging {percentage}%"));
            }
            if ui.button("Cancel Windows preparation").clicked() {
                self.later();
            }
        }
        if let Some(v) = self.view.version {
            ui.label(format!("Prepared native package {}", version_text(v)));
        }
        if let Some(v) = self.view.staged_version {
            ui.label(format!(
                "{} native package {}",
                if self.view.recorded_staging {
                    "Recorded staging result for"
                } else {
                    "Staged"
                },
                version_text(v)
            ));
        }
        if let Some(v) = self.view.installed_version {
            ui.label(format!(
                "Current registered native package {}",
                version_text(v)
            ));
        }
        if let Some(v) = self.view.observed_target {
            ui.label(format!(
                "Observed selected target {} · health unverified",
                version_text(v)
            ));
        }
        if !self.view.message.is_empty() {
            ui.label(&self.view.message);
        }
    }
    #[cfg(test)]
    pub(crate) fn fake_staging(
        &mut self,
        uncertain: bool,
    ) -> (mpsc::Sender<()>, mpsc::Receiver<()>) {
        let (tx, rx) = mpsc::channel();
        let (finished, done) = mpsc::channel();
        let root = self.root.clone();
        let profile = self.profile.clone();
        self.next_stage = Some(Box::new(move |control| {
            let result = (|| {
                if let Some(profile) = &profile {
                    profile.validate()?;
                }
                tests::stage_fixture(&root, control, rx, uncertain)
            })();
            drop(profile);
            let _ = finished.send(());
            result
        }));
        (tx, done)
    }
    #[cfg(test)]
    pub(crate) fn fake_reconciliation(
        &mut self,
    ) -> (mpsc::Sender<()>, mpsc::Receiver<()>, mpsc::Receiver<()>) {
        let (release, resume) = mpsc::channel();
        let (entered, ready) = mpsc::channel();
        let (finished, done) = mpsc::channel();
        let root = self.root.clone();
        let profile = self.staging_profile();
        self.next_reconcile = Some(Box::new(move |control| {
            let result = (|| {
                let profile = profile?;
                profile.validate()?;
                tests::inspect_fixture(&root, control, resume, entered)
            })();
            let _ = finished.send(());
            result
        }));
        (release, ready, done)
    }
    #[cfg(test)]
    pub(crate) fn fake_prepare(&mut self) -> mpsc::Sender<()> {
        let (tx, rx) = mpsc::channel();
        self.next = Some(Box::new(move |control| {
            control.phase("Verifying synthetic signed Windows media", 12345)?;
            rx.recv_timeout(Duration::from_secs(5))
                .map_err(|_| "Synthetic Windows operation was not released.")?;
            control.check()?;
            Ok(tests::receipt())
        }));
        tx
    }
}
impl Drop for Controller {
    fn drop(&mut self) {
        self.later();
    }
}
