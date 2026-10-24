//! Release discovery and dormant signed media preparation. No installer is executed.
#[cfg(any(windows, test))]
pub(crate) mod windows;
use semver::Version;
use serde::{Deserialize, Serialize};
use std::{
    io::Read,
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

const ENDPOINT: &str = "https://api.github.com/repos/Deftr0y/PeerBrush/releases?per_page=100";
pub const RESPONSE_LIMIT: usize = 2 * 1024 * 1024;
pub const CHECK_INTERVAL: u64 = 24 * 60 * 60;

#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub enum Owner {
    MicrosoftStore,
    WindowsAppInstaller,
    WindowsPackage,
    LinuxPackage,
    Direct,
    Unknown,
}
impl Owner {
    pub fn current() -> Self {
        #[cfg(windows)]
        {
            use ::windows::ApplicationModel::Package;
            match crate::packaged::package_family() {
                Ok(None) => Self::Direct,
                Ok(Some(_)) => match Package::Current() {
                    Ok(package) => windows_package_owner(package.SignatureKind(), || {
                        // Association only; never read/expose its potentially private URI.
                        package.GetAppInstallerInfo().is_ok()
                    }),
                    Err(_) => Self::Unknown,
                },
                Err(_) => Self::Unknown,
            }
        }
        #[cfg(target_os = "linux")]
        {
            // Conservatively leave system-installed binaries with their package
            // manager. This does not claim a configured automatic-update repository.
            if Path::new("/.flatpak-info").is_file()
                || std::env::current_exe().is_ok_and(|p| {
                    p == Path::new("/usr/bin/peerbrush")
                        || p.starts_with("/usr/lib/peerbrush")
                        || p.starts_with("/opt/peerbrush")
                })
            {
                Self::LinuxPackage
            } else {
                Self::Direct
            }
        }
        #[cfg(target_os = "macos")]
        {
            Self::Direct
        }
        #[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
        {
            Self::Unknown
        }
    }
    pub fn description(self) -> &'static str {
        match self {
            Self::MicrosoftStore => "Microsoft Store manages this installation's updates.",
            Self::WindowsAppInstaller => "Windows App Installer is associated with this installation. Its update settings and availability have not been verified.",
            Self::WindowsPackage => "Windows package installation; Store origin is not established. Use its installation provider for updates.",
            Self::LinuxPackage => "System/package-managed installation. Use its package manager; an automatic-update repository may not be configured.",
            Self::Direct => "Direct distribution or local build. Automatic installation is not configured.",
            Self::Unknown => "Installation owner could not be verified. Automatic installation is unavailable.",
        }
    }
}

#[cfg(windows)]
fn windows_package_owner(
    signature: ::windows::core::Result<::windows::ApplicationModel::PackageSignatureKind>,
    app_installer_associated: impl FnOnce() -> bool,
) -> Owner {
    use ::windows::ApplicationModel::PackageSignatureKind;
    match signature {
        Ok(PackageSignatureKind::Store) => Owner::MicrosoftStore,
        Ok(PackageSignatureKind::Developer | PackageSignatureKind::Enterprise) => {
            if app_installer_associated() {
                Owner::WindowsAppInstaller
            } else {
                Owner::WindowsPackage
            }
        }
        Ok(_) => Owner::WindowsPackage,
        Err(_) => Owner::Unknown,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Channel {
    #[default]
    Stable,
    Preview,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Preferences {
    pub version: u32,
    pub automatic_checks: bool,
    pub channel: Channel,
    pub last_attempt: Option<u64>,
}
impl Default for Preferences {
    fn default() -> Self {
        Self {
            version: 1,
            automatic_checks: false,
            channel: Channel::Stable,
            last_attempt: None,
        }
    }
}
pub struct Store {
    pub preferences: Preferences,
    path: Option<PathBuf>,
    original: Option<Vec<u8>>,
    pub error: Option<String>,
}
impl Default for Store {
    fn default() -> Self {
        Self {
            preferences: Preferences::default(),
            path: None,
            original: None,
            error: None,
        }
    }
}
impl Store {
    pub fn load(directory: &Path) -> Self {
        let mut store = Self::default();
        if directory.as_os_str().is_empty() {
            return store;
        }
        let path = directory.join("updates.json");
        let result = (|| {
            store.original = crate::tool_settings::read_owned(&path)?;
            if let Some(bytes) = &store.original {
                let preferences = decode_preferences(bytes)?;
                if crate::tool_settings::read_owned(&path)? != store.original {
                    return Err("Update preferences changed while loading".into());
                }
                store.preferences = preferences;
            }
            Ok(())
        })();
        store.path = Some(path);
        store.error = result.err();
        store
    }
    pub fn save(&mut self, preferences: Preferences) -> Result<(), String> {
        if let Some(error) = &self.error {
            return Err(format!("Update preferences remain protected: {error}"));
        }
        let bytes = serde_json::to_vec_pretty(&preferences).map_err(|e| e.to_string())?;
        decode_preferences(&bytes)?;
        if let Some(path) = &self.path {
            let lease = crate::file_safety::Lease::acquire(path)?;
            if crate::tool_settings::read_owned(path)? != self.original {
                return Err(
                    "Update preferences changed on disk; reopen PeerBrush before saving".into(),
                );
            }
            let backup = lease.preserve()?;
            crate::server::atomic_write_checked(path, &bytes, |_| lease.validate())?;
            if let Some(backup) = backup {
                lease.prune(&backup);
            }
            self.original = Some(bytes);
        }
        self.preferences = preferences;
        Ok(())
    }
    pub fn due(&self, now: u64) -> bool {
        self.error.is_none()
            && self.preferences.automatic_checks
            && self
                .preferences
                .last_attempt
                .is_none_or(|last| now.saturating_sub(last) >= CHECK_INTERVAL)
    }
}
fn decode_preferences(bytes: &[u8]) -> Result<Preferences, String> {
    let preferences: Preferences =
        serde_json::from_slice(bytes).map_err(|e| format!("Unreadable update preferences: {e}"))?;
    if preferences.version != 1 {
        return Err("Unsupported update preferences version".into());
    }
    Ok(preferences)
}
pub(crate) fn validate_preferences(path: &Path) -> Result<(), String> {
    if let Some(bytes) = crate::tool_settings::read_owned(path)? {
        decode_preferences(&bytes)?;
    }
    Ok(())
}
pub fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub enum Target {
    WindowsX64,
    LinuxX64,
    MacArm64,
}
impl Target {
    pub fn current() -> Result<Self, String> {
        match (std::env::consts::OS, std::env::consts::ARCH) {
            ("windows", "x86_64") => Ok(Self::WindowsX64),
            ("linux", "x86_64") => Ok(Self::LinuxX64),
            ("macos", "aarch64") => Ok(Self::MacArm64),
            _ => Err("No supported release architecture is configured for this build".into()),
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Self::WindowsX64 => "Windows x64",
            Self::LinuxX64 => "Linux x64",
            Self::MacArm64 => "macOS arm64",
        }
    }
    fn accepts(self, name: &str, version: &str) -> bool {
        match self {
            Self::WindowsX64 => name == format!("PeerBrush-{version}-Windows-x64.zip"),
            Self::LinuxX64 => {
                name == format!("PeerBrush-{version}-Linux-x64.zip")
                    || (!version.contains('+')
                        && name
                            == format!("peerbrush_{}-1_amd64.deb", version.replacen('-', "~", 1)))
            }
            Self::MacArm64 => {
                let prefix = format!("PeerBrush-{version}-macOS-arm64");
                name == format!("{prefix}.zip")
                    || name
                        .strip_prefix(&format!("{prefix}-apple-signed-r"))
                        .and_then(|s| s.strip_suffix(".zip"))
                        .is_some_and(revision)
                    || name
                        .strip_prefix(&format!("{prefix}-apple-signed-r"))
                        .and_then(|s| s.strip_suffix(".dmg"))
                        .and_then(|s| s.split_once("-dmg-signed-r"))
                        .is_some_and(|(app, image)| revision(app) && revision(image))
            }
        }
    }
}
fn revision(value: &str) -> bool {
    value.len() <= 6
        && value
            .parse::<u32>()
            .is_ok_and(|r| r > 0 && r.to_string() == value)
}
#[derive(Clone, Debug, Serialize)]
pub struct PublishedAsset {
    pub name: String,
    pub bytes: u64,
}
#[derive(Clone, Debug, Serialize)]
pub struct Report {
    pub current_version: String,
    pub channel: Channel,
    pub target: Target,
    pub release_version: Option<String>,
    pub release_url: Option<String>,
    pub notes: String,
    pub assets: Vec<PublishedAsset>,
    pub checked_at: u64,
}
#[derive(Clone, Debug)]
pub struct Progress {
    pub stage: &'static str,
    pub bytes: usize,
}

/// The endpoint is fixed, HTTPS certificate/hostname validation is always enabled,
/// and redirects are disabled. Returned asset links are never fetched or executed.
pub fn check(
    channel: Channel,
    target: Target,
    progress: impl Fn(Progress),
) -> Result<Report, String> {
    progress(Progress {
        stage: "Connecting securely to GitHub",
        bytes: 0,
    });
    let agent = ureq::AgentBuilder::new()
        .https_only(true)
        .redirects(0)
        .timeout_connect(Duration::from_secs(5))
        .timeout_read(Duration::from_secs(5))
        .timeout(Duration::from_secs(25))
        .build();
    let response = agent.get(ENDPOINT)
        .set("Accept", "application/vnd.github+json")
        .set("X-GitHub-Api-Version", "2022-11-28")
        .set("User-Agent", concat!("PeerBrush/", env!("CARGO_PKG_VERSION")))
        .call()
        .map_err(|error| match error {
            ureq::Error::Status(code, _) => format!("Official release check returned HTTP {code}; no update status is available. Try again later."),
            _ => "Could not securely reach GitHub. Check your connection and try again; no update status is available.".into(),
        })?;
    if response.status() != 200 || response.get_url() != ENDPOINT {
        return Err(
            "Unexpected release response or redirect; no update status is available".into(),
        );
    }
    let content_type = response.header("Content-Type").unwrap_or("");
    if content_type.split(';').next().map(str::trim) != Some("application/json") {
        return Err("Official release response is not JSON".into());
    }
    if response
        .header("Content-Length")
        .and_then(|s| s.parse::<usize>().ok())
        .is_some_and(|n| n > RESPONSE_LIMIT)
    {
        return Err("Official release response exceeds 2 MiB".into());
    }
    let bytes = read_response(response.into_reader(), &progress)?;
    progress(Progress {
        stage: "Checking published versions and architecture",
        bytes: bytes.len(),
    });
    parse_releases(&bytes, env!("CARGO_PKG_VERSION"), channel, target, now())
}
fn read_response(mut reader: impl Read, progress: &impl Fn(Progress)) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::new();
    let mut buffer = [0; 32 * 1024];
    loop {
        let count = reader
            .read(&mut buffer)
            .map_err(|_| "Release response was interrupted; retry the check")?;
        if count == 0 {
            return Ok(bytes);
        }
        if bytes.len() + count > RESPONSE_LIMIT {
            return Err("Official release response exceeds 2 MiB".into());
        }
        bytes.extend_from_slice(&buffer[..count]);
        progress(Progress {
            stage: "Reading release information",
            bytes: bytes.len(),
        });
    }
}
#[derive(Deserialize)]
struct Release {
    draft: bool,
    prerelease: bool,
    tag_name: String,
    html_url: String,
    published_at: Option<String>,
    body: Option<String>,
    assets: Vec<Asset>,
}
#[derive(Deserialize)]
struct Asset {
    name: String,
    size: u64,
    state: String,
    browser_download_url: String,
}
fn canonical_url(actual: &str, expected: &str) -> bool {
    url::Url::parse(actual).is_ok_and(|u| u.as_str() == expected && actual == expected)
}
pub fn parse_releases(
    bytes: &[u8],
    current: &str,
    channel: Channel,
    target: Target,
    checked_at: u64,
) -> Result<Report, String> {
    if bytes.len() > RESPONSE_LIMIT {
        return Err("Official release response exceeds 2 MiB".into());
    }
    let releases: Vec<Release> =
        serde_json::from_slice(bytes).map_err(|_| "Invalid official release response")?;
    if releases.len() > 100 {
        return Err("Official release collection exceeds 100 entries".into());
    }
    let current_version =
        Version::parse(current).map_err(|_| "Invalid current application version")?;
    let mut candidates = Vec::new();
    for release in releases {
        if release.draft || release.published_at.is_none() {
            continue;
        }
        if release.tag_name.len() > 80 || release.assets.len() > 64 {
            return Err("Oversized release metadata".into());
        }
        let Ok(version) = Version::parse(
            release
                .tag_name
                .strip_prefix('v')
                .unwrap_or(&release.tag_name),
        ) else {
            continue;
        };
        if channel == Channel::Stable && (release.prerelease || !version.pre.is_empty()) {
            continue;
        }
        if version.cmp_precedence(&current_version).is_gt() {
            candidates.push((version, release));
        }
    }
    candidates.sort_by(|a, b| b.0.cmp_precedence(&a.0));
    let mut report = Report {
        current_version: current.into(),
        channel,
        target,
        release_version: None,
        release_url: None,
        notes: String::new(),
        assets: vec![],
        checked_at,
    };
    if let Some((version, release)) = candidates.into_iter().next() {
        let expected = format!(
            "https://github.com/Deftr0y/PeerBrush/releases/tag/{}",
            release.tag_name
        );
        if !canonical_url(&release.html_url, &expected) {
            return Err("Release URL does not match the official repository".into());
        }
        for asset in release.assets {
            if asset.name.len() > 160
                || !asset
                    .name
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"-_.+~".contains(&b))
            {
                return Err("Invalid release asset name".into());
            }
            let expected = format!(
                "https://github.com/Deftr0y/PeerBrush/releases/download/{}/{}",
                release.tag_name, asset.name
            );
            if !canonical_url(&asset.browser_download_url, &expected) {
                return Err("Asset URL does not match the official release".into());
            }
            if asset.state == "uploaded"
                && asset.size > 0
                && asset.size <= 2 * 1024 * 1024 * 1024
                && target.accepts(&asset.name, &version.to_string())
            {
                report.assets.push(PublishedAsset {
                    name: asset.name,
                    bytes: asset.size,
                });
            }
        }
        let notes = release.body.unwrap_or_default();
        report.notes = notes.chars().take(8192).collect();
        if notes.chars().count() > 8192 {
            report
                .notes
                .push_str("\n\nNotes truncated; read the full official release page.");
        }
        report.release_version = Some(version.to_string());
        report.release_url = Some(release.html_url);
    }
    Ok(report)
}

#[cfg(test)]
#[path = "updates_tests.rs"]
mod tests;
