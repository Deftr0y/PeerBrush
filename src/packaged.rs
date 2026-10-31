//! Stable external launch paths for the optional Windows MSIX distribution.
//! Portable builds continue to use their own executable, without package state.
use std::path::{Path, PathBuf};

pub const MCP_ALIAS: &str = "peerbrush-mcp.exe";

/// Public, read-only process observations for installed qualification. This is
/// not a signature verifier, installation receipt, or an update permission.
pub fn installation_info() -> Result<serde_json::Value, String> {
    #[cfg(windows)]
    let package = installation_package()?;
    #[cfg(not(windows))]
    let package = None::<serde_json::Value>;
    let build: serde_json::Value = serde_json::from_str(crate::build_info::json())
        .map_err(|_| "Cannot identify this build")?;
    let observation = serde_json::json!({
        "schema": 1,
        "format": "PeerBrushInstallationObservationV1",
        "platform": std::env::consts::OS,
        "build": build,
        "package": package,
        "installed_acceptance": "not-established",
        "update_eligible": false
    });
    if serde_json::to_vec(&observation)
        .map_err(|_| "Cannot encode installation observation")?
        .len()
        > 16 * 1024
    {
        return Err("Installation observation exceeds its bounded output".into());
    }
    Ok(observation)
}

#[cfg(any(windows, test))]
fn installation_route(
    kind: &str,
    associated: bool,
    development: bool,
    non_main: bool,
) -> &'static str {
    if development || non_main {
        "ineligible"
    } else if kind == "store" {
        "microsoft-store"
    } else if matches!(kind, "developer" | "enterprise") {
        if associated {
            "legacy-appinstaller"
        } else {
            "verified-local-candidate"
        }
    } else {
        "ineligible"
    }
}

#[cfg(windows)]
fn installation_package() -> Result<Option<serde_json::Value>, String> {
    use windows::{
        ApplicationModel::{Package, PackageSignatureKind},
        Win32::System::Com::{CoInitializeEx, CoUninitialize, COINIT_MULTITHREADED},
    };
    let Some(family) = package_family()? else {
        return Ok(None);
    };
    let native_error = |_| "Cannot read the current Windows package observation".to_owned();
    unsafe { CoInitializeEx(None, COINIT_MULTITHREADED).ok() }.map_err(native_error)?;
    struct Apartment;
    impl Drop for Apartment {
        fn drop(&mut self) {
            unsafe { CoUninitialize() };
        }
    }
    let _apartment = Apartment;
    let package = Package::Current().map_err(native_error)?;
    let id = package.Id().map_err(native_error)?;
    if id.FamilyName().map_err(native_error)?.to_string() != family {
        return Err("Current Windows package identities disagree".into());
    }
    let signature = package.SignatureKind().map_err(native_error)?;
    let kind = match signature {
        PackageSignatureKind::Developer => "developer",
        PackageSignatureKind::Enterprise => "enterprise",
        PackageSignatureKind::Store => "store",
        PackageSignatureKind::System => "system",
        _ => "untrusted-or-unknown",
    };
    let associated = appinstaller_association(&package)
        .map_err(native_error)?
        .is_some();
    let development = package.IsDevelopmentMode().map_err(native_error)?;
    let non_main = package.IsFramework().map_err(native_error)?
        || package.IsResourcePackage().map_err(native_error)?
        || package.IsOptional().map_err(native_error)?;
    let version = id.Version().map_err(native_error)?;
    Ok(Some(serde_json::json!({
        "name": id.Name().map_err(native_error)?.to_string(),
        "publisher": id.Publisher().map_err(native_error)?.to_string(),
        "family": family,
        "full_name": id.FullName().map_err(native_error)?.to_string(),
        "version": [version.Major, version.Minor, version.Build, version.Revision],
        "signature_kind": kind,
        "appinstaller_association": associated,
        "development_mode": development,
        "non_main": non_main,
        "candidate_route": installation_route(kind, associated, development, non_main)
    })))
}

// The WinRT projection maps successful NULL to an error. Preserve native
// successful absence separately from failed queries, without exposing a URI.
#[cfg(windows)]
pub(crate) fn appinstaller_association(
    package: &windows::ApplicationModel::Package,
) -> windows::core::Result<Option<windows::ApplicationModel::AppInstallerInfo>> {
    use windows::{
        core::Interface,
        ApplicationModel::{AppInstallerInfo, IPackage6},
    };
    let interface: IPackage6 = package.cast()?;
    let mut pointer = std::ptr::null_mut();
    unsafe {
        (Interface::vtable(&interface).GetAppInstallerInfo)(
            Interface::as_raw(&interface),
            &mut pointer,
        )
        .ok()?;
    }
    Ok(if pointer.is_null() {
        None
    } else {
        Some(unsafe { AppInstallerInfo::from_raw(pointer) })
    })
}

/// Installed state must be writable, upgrade-stable, and visible to external
/// local agents reading the discovery manifest. Keep portable defaults intact.
pub fn default_state_dir(portable: PathBuf) -> Result<PathBuf, String> {
    #[cfg(windows)]
    if package_family()?.is_some() {
        let home = std::env::var_os("USERPROFILE")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .ok_or("Cannot locate the user profile for installed PeerBrush state")?;
        return Ok(home.join(".peerbrush/state"));
    }
    Ok(portable)
}

/// Never persist the versioned, protected WindowsApps installation directory in
/// an AI client's configuration. A family-scoped alias also avoids PATH lookup
/// and collisions with aliases registered by another package.
pub fn mcp_command() -> Result<PathBuf, String> {
    let executable = std::env::current_exe().map_err(|e| e.to_string())?;
    #[cfg(windows)]
    {
        if let Some(family) = package_family()? {
            let local = std::env::var_os("LOCALAPPDATA")
                .map(PathBuf::from)
                .ok_or("Cannot locate Local AppData for the installed MCP alias")?;
            return command_path(&executable, Some((&local, &family)));
        }
    }
    command_path(&executable, None)
}

/// Package writes under AppData can be private to the package. Never claim an
/// external client was configured when it cannot see the resulting file.
pub fn external_config_is_visible(path: &Path) -> Result<bool, String> {
    #[cfg(windows)]
    {
        if package_family()?.is_some() {
            let home = std::env::var_os("USERPROFILE")
                .map(PathBuf::from)
                .filter(|p| p.is_absolute())
                .ok_or("Cannot locate the user profile for package integration")?;
            let local = std::env::var_os("LOCALAPPDATA").map(PathBuf::from);
            let roaming = std::env::var_os("APPDATA").map(PathBuf::from);
            // An uncertain destination affects this client only. It must use
            // manual setup rather than prevent other clients from connecting.
            return Ok(effective_config_visibility(
                path,
                &home,
                local.as_deref(),
                roaming.as_deref(),
            )
            .unwrap_or(false));
        }
    }
    let _ = path;
    Ok(true)
}

/// Resolve the actual destination, including symlinked files and directory
/// junctions. For a new file, resolve its nearest existing parent first. Never
/// treat a dangling link or an inaccessible parent as an ordinary new path.
#[cfg(any(windows, test))]
fn effective_destination(path: &Path) -> Result<PathBuf, String> {
    if !path.is_absolute() {
        return Err("Configuration destination must be absolute".into());
    }
    let mut existing = path;
    let mut suffix = Vec::new();
    loop {
        match std::fs::canonicalize(existing) {
            Ok(mut resolved) => {
                if !suffix.is_empty() && !resolved.is_dir() {
                    return Err("Cannot resolve configuration destination safely".into());
                }
                for part in suffix.iter().rev() {
                    resolved.push(part);
                }
                return Ok(resolved);
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                match std::fs::symlink_metadata(existing) {
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                    _ => return Err("Cannot resolve configuration destination safely".into()),
                }
            }
            Err(_) => return Err("Cannot resolve configuration destination safely".into()),
        }
        suffix.push(
            existing
                .file_name()
                .ok_or("Cannot resolve configuration parent")?
                .to_owned(),
        );
        existing = existing.parent().ok_or("Configuration needs a parent")?;
    }
}

#[cfg(any(windows, test))]
fn effective_config_visibility(
    path: &Path,
    home: &Path,
    local: Option<&Path>,
    roaming: Option<&Path>,
) -> Result<bool, String> {
    let path = effective_destination(path)?;
    let appdata = effective_destination(&home.join("AppData"))?;
    let local = local.map(effective_destination).transpose()?;
    let roaming = roaming.map(effective_destination).transpose()?;
    let in_appdata = within(&path, &appdata)
        || local.as_ref().is_some_and(|p| within(&path, p))
        || roaming.as_ref().is_some_and(|p| within(&path, p));
    // No restricted virtualization exemptions: clients under AppData require
    // manual setup on every supported Windows version.
    Ok(!in_appdata)
}

#[cfg(any(windows, test))]
fn comparable_path(path: &Path) -> String {
    let path = path.to_string_lossy().replace('/', "\\");
    // canonicalize returns verbatim paths on Windows. Compare those against
    // ordinary environment paths without confusing a UNC prefix with a drive.
    let path = path
        .strip_prefix("\\\\?\\")
        .or_else(|| path.strip_prefix("\\??\\"))
        .unwrap_or(&path);
    let path = if path
        .get(..4)
        .is_some_and(|p| p.eq_ignore_ascii_case("UNC\\"))
    {
        format!("\\\\{}", &path[4..])
    } else {
        path.to_owned()
    };
    path.trim_end_matches('\\').to_owned()
}

#[cfg(test)]
fn same_path(a: &Path, b: &Path) -> bool {
    comparable_path(a).eq_ignore_ascii_case(&comparable_path(b))
}

#[cfg(any(windows, test))]
fn within(path: &Path, directory: &Path) -> bool {
    let path = comparable_path(path);
    let directory = comparable_path(directory);
    path.eq_ignore_ascii_case(&directory)
        || (path
            .get(..directory.len())
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case(&directory))
            && path.as_bytes().get(directory.len()) == Some(&b'\\'))
}

fn command_path(executable: &Path, package: Option<(&Path, &str)>) -> Result<PathBuf, String> {
    let Some((local, family)) = package else {
        return Ok(executable.to_owned());
    };
    let alias = family_alias_path(local, family)?;
    // App execution aliases are APPEXECLINK reparse points, not binary copies.
    // Inspect the entry's tag; never canonicalize, copy, or read its bytes.
    #[cfg(windows)]
    let available = is_app_execution_alias(&alias);
    #[cfg(not(windows))]
    let available = alias.is_file(); // Portable test fixture only; no MSIX on this OS.
    if !available {
        return Err("PeerBrush's installed MCP alias is unavailable. Check Windows Settings > Apps > Advanced app settings > App execution aliases, then retry Connect AI.".into());
    }
    Ok(alias)
}

fn family_alias_path(local: &Path, family: &str) -> Result<PathBuf, String> {
    if !local.is_absolute()
        || family.is_empty()
        || !family
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "._-".contains(c))
    {
        return Err("Invalid package identity or Local AppData location".into());
    }
    Ok(local
        .join("Microsoft/WindowsApps")
        .join(family)
        .join(MCP_ALIAS))
}

/// Keep ordinary executable validation strict while allowing Windows' special
/// app execution link type. Ordinary/dangling symlinks are not execution aliases.
pub fn is_app_execution_alias(path: &Path) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::{fs::OpenOptionsExt, io::AsRawHandle};
        #[repr(C)]
        struct AttributeTag {
            attributes: u32,
            tag: u32,
        }
        #[link(name = "kernel32")]
        extern "system" {
            fn GetFileInformationByHandleEx(
                file: *mut std::ffi::c_void,
                class: i32,
                info: *mut std::ffi::c_void,
                size: u32,
            ) -> i32;
        }
        const OPEN_REPARSE_POINT: u32 = 0x00200000;
        const BACKUP_SEMANTICS: u32 = 0x02000000;
        const FILE_ATTRIBUTE_TAG_INFO: i32 = 9;
        const APPEXECLINK: u32 = 0x8000001b;
        let Ok(file) = std::fs::OpenOptions::new()
            .access_mode(0)
            .share_mode(7)
            .custom_flags(OPEN_REPARSE_POINT | BACKUP_SEMANTICS)
            .open(path)
        else {
            return false;
        };
        let mut info = AttributeTag {
            attributes: 0,
            tag: 0,
        };
        // Only metadata is queried from a handle opened without following the
        // reparse point. The OS owns the link and handles activation separately.
        let success = unsafe {
            GetFileInformationByHandleEx(
                file.as_raw_handle(),
                FILE_ATTRIBUTE_TAG_INFO,
                (&mut info as *mut AttributeTag).cast(),
                std::mem::size_of::<AttributeTag>() as u32,
            )
        };
        return success != 0
            && info.attributes & 0x400 != 0
            && info.attributes & 0x10 == 0
            && info.tag == APPEXECLINK;
    }
    #[cfg(not(windows))]
    {
        let _ = path;
        false
    }
}

#[cfg(windows)]
pub(crate) fn package_family() -> Result<Option<String>, String> {
    #[link(name = "kernel32")]
    extern "system" {
        fn GetCurrentPackageFamilyName(length: *mut u32, name: *mut u16) -> i32;
    }
    const NO_PACKAGE: i32 = 15700;
    const INSUFFICIENT_BUFFER: i32 = 122;
    let mut length = 0;
    // The API first reports the required UTF-16 buffer length, including NUL.
    let result = unsafe { GetCurrentPackageFamilyName(&mut length, std::ptr::null_mut()) };
    if result == NO_PACKAGE {
        return Ok(None);
    }
    if result != INSUFFICIENT_BUFFER || !(2..=1024).contains(&length) {
        return Err("Could not determine the Windows package identity".into());
    }
    let mut name = vec![0u16; length as usize];
    let result = unsafe { GetCurrentPackageFamilyName(&mut length, name.as_mut_ptr()) };
    if result != 0 || length < 2 || length as usize > name.len() {
        return Err("Could not read the Windows package identity".into());
    }
    String::from_utf16(&name[..length as usize - 1])
        .map(Some)
        .map_err(|_| "Invalid Windows package identity".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn package_route_observations_never_grant_installation_or_update_authority() {
        for kind in ["developer", "enterprise"] {
            assert_eq!(
                installation_route(kind, false, false, false),
                "verified-local-candidate"
            );
            assert_eq!(
                installation_route(kind, true, false, false),
                "legacy-appinstaller"
            );
        }
        assert_eq!(
            installation_route("store", false, false, false),
            "microsoft-store"
        );
        for kind in [
            "developer",
            "enterprise",
            "store",
            "system",
            "untrusted-or-unknown",
        ] {
            assert_eq!(installation_route(kind, false, true, false), "ineligible");
            assert_eq!(installation_route(kind, false, false, true), "ineligible");
        }
        assert_eq!(
            installation_route("untrusted-or-unknown", false, false, false),
            "ineligible"
        );
        let observation = installation_info().unwrap();
        assert_eq!(observation["format"], "PeerBrushInstallationObservationV1");
        assert_eq!(observation["installed_acceptance"], "not-established");
        assert_eq!(observation["update_eligible"], false);
        assert!(observation["package"].is_null()); // Test runner is unpackaged.
    }

    #[test]
    fn portable_command_stays_unchanged() {
        let exe = Path::new("portable folder/peerbrush.exe");
        assert_eq!(command_path(exe, None).unwrap(), exe);
    }

    #[test]
    fn family_alias_is_stable_across_package_versions_and_independent_of_path() {
        let dir = std::env::temp_dir().join(format!("peerbrush-alias-{}", uuid::Uuid::new_v4()));
        let family = "PeerBrush.Test_0123456789abc";
        let alias = dir
            .join("Microsoft/WindowsApps")
            .join(family)
            .join(MCP_ALIAS);
        std::fs::create_dir_all(alias.parent().unwrap()).unwrap();
        std::fs::write(&alias, []).unwrap();
        // Construction depends on the family, never the versioned executable.
        assert_eq!(family_alias_path(&dir, family).unwrap(), alias);
        #[cfg(not(windows))]
        for exe in [
            "WindowsApps/PeerBrush_1.0.0.0/peerbrush.exe",
            "WindowsApps/PeerBrush_1.0.1.0/peerbrush.exe",
        ] {
            assert_eq!(
                command_path(Path::new(exe), Some((&dir, family))).unwrap(),
                alias
            );
        }
        // A regular file must not masquerade as the installed Windows alias.
        assert!(!is_app_execution_alias(&alias));
        #[cfg(windows)]
        assert!(command_path(Path::new("peerbrush.exe"), Some((&dir, family))).is_err());
        std::fs::remove_dir_all(&dir).unwrap();
        assert!(
            command_path(Path::new("old/version/peerbrush.exe"), Some((&dir, family))).is_err()
        );
    }

    #[test]
    fn malformed_family_or_relative_appdata_fails_closed() {
        for family in ["", "../escape", "family/escape", "family\\escape"] {
            assert!(command_path(
                Path::new("peerbrush.exe"),
                Some((Path::new("/tmp"), family))
            )
            .is_err());
        }
        assert!(command_path(
            Path::new("peerbrush.exe"),
            Some((Path::new("relative"), "Family_123"))
        )
        .is_err());
    }

    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            let path =
                std::env::temp_dir().join(format!("peerbrush-visibility-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(path.join("AppData/Local")).unwrap();
            std::fs::create_dir_all(path.join("AppData/Roaming/Claude")).unwrap();
            Self(path)
        }
        fn visible(&self, path: &Path) -> bool {
            effective_config_visibility(
                path,
                &self.0,
                Some(&self.0.join("AppData/Local")),
                Some(&self.0.join("AppData/Roaming")),
            )
            .unwrap_or(false)
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn existing_and_new_destinations_resolve_before_visibility_checks() {
        let f = Fixture::new();
        let target = f.0.join("AppData/Local/config.toml");
        std::fs::write(&target, "").unwrap();
        assert_eq!(
            effective_destination(&target).unwrap(),
            std::fs::canonicalize(&target).unwrap()
        );
        let new_path = f.0.join("AppData/Local/new/nested/config.toml");
        assert_eq!(
            effective_destination(&new_path).unwrap(),
            std::fs::canonicalize(f.0.join("AppData/Local"))
                .unwrap()
                .join("new/nested/config.toml")
        );
        assert!(!f.visible(&target));
        assert!(!f.visible(&new_path));
        assert!(f.visible(&f.0.join(".codex/config.toml")));
        let claude =
            f.0.join("AppData/Roaming/Claude/claude_desktop_config.json");
        assert!(!f.visible(&claude));
        assert!(!f.visible(&f.0.join("AppData/Roaming/Claude-other/config.json")));
        assert!(!f.visible(&f.0.join("AppData/Local/../Roaming/custom/config.json")));
        assert!(effective_destination(Path::new("relative/config.toml")).is_err());
        assert!(effective_destination(&target.join("not-a-directory/config.toml")).is_err());
    }

    #[test]
    fn redirected_roaming_remains_manual() {
        let f = Fixture::new();
        let roaming = f.0.join("redirected-roaming");
        std::fs::create_dir_all(&roaming).unwrap();
        assert!(!effective_config_visibility(
            &roaming.join("Claude/config.json"),
            &f.0,
            Some(&f.0.join("AppData/Local")),
            Some(&roaming),
        )
        .unwrap());
    }

    #[test]
    fn windows_verbatim_prefixes_and_directory_boundaries_compare_correctly() {
        assert!(same_path(
            Path::new(r"\\?\C:\Users\Me\AppData\Roaming"),
            Path::new(r"c:/users/me/AppData/Roaming/")
        ));
        assert!(same_path(
            Path::new(r"\\?\UNC\server\share\AppData"),
            Path::new(r"\\server\share\AppData")
        ));
        assert!(within(
            Path::new(r"\\?\C:\Users\Me\AppData\Roaming\Claude\config.json"),
            Path::new(r"C:\Users\Me\AppData\Roaming\Claude")
        ));
        assert!(!within(
            Path::new(r"C:\Users\Me\AppData\Roaming\Claude-other\config.json"),
            Path::new(r"C:\Users\Me\AppData\Roaming\Claude")
        ));
    }

    #[cfg(unix)]
    #[test]
    fn file_and_parent_symlinks_cannot_hide_appdata_destinations() {
        use std::os::unix::fs::symlink;
        let f = Fixture::new();
        let target = f.0.join("AppData/Local/config.toml");
        std::fs::write(&target, "").unwrap();
        let link = f.0.join("config.toml");
        symlink(&target, &link).unwrap();
        assert_eq!(
            effective_destination(&link).unwrap(),
            std::fs::canonicalize(&target).unwrap()
        );
        assert!(!f.visible(&link));
        let directory = f.0.join(".codex");
        symlink(f.0.join("AppData/Local"), &directory).unwrap();
        assert!(!f.visible(&directory.join("config.toml")));
        assert!(!f.visible(&directory.join("new/nested/config.toml")));
        let dangling = f.0.join("dangling");
        symlink(f.0.join("absent"), &dangling).unwrap();
        assert!(effective_destination(&dangling).is_err());
        assert!(effective_destination(&dangling.join("config.toml")).is_err());
        assert!(!f.visible(&dangling.join("config.toml")));
        assert!(!is_app_execution_alias(&dangling));
    }

    #[cfg(unix)]
    #[test]
    fn claude_directory_symlink_remains_manual() {
        use std::os::unix::fs::symlink;
        let f = Fixture::new();
        let claude = f.0.join("AppData/Roaming/Claude");
        std::fs::remove_dir(&claude).unwrap();
        symlink(f.0.join("AppData/Local"), &claude).unwrap();
        assert!(!f.visible(&claude.join("config.json")));
        assert!(f.visible(&f.0.join(".codex/config.toml")));
    }
}
