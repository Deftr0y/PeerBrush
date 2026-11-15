//! Local Caskroom/app observations, never publisher verification or brew execution.
use crate::updates::Owner;
use serde::Deserialize;
use std::{
    fs,
    io::Read,
    path::{Component, Path, PathBuf},
};

const LIMIT: u64 = 128 * 1024;
const OBSERVATION_ERROR: &str = "Homebrew's PeerBrush records are unavailable or inconsistent. Package ownership and update availability remain unverified.";
const LAUNCHER_ERROR: &str = "Homebrew's PeerBrush command does not match this running app. Open the version selected by Homebrew, then retry Connect AI; client settings were not changed.";

#[derive(Deserialize)]
struct Receipt {
    source: Source,
    arch: String,
}
#[derive(Deserialize)]
struct Source {
    version: String,
}
struct Installation {
    executable: PathBuf,
    launcher: PathBuf,
}

pub(crate) fn owner(executable: Option<&Path>) -> Owner {
    let Some(executable) = executable else {
        return Owner::Unknown;
    };
    classify(observe(executable, &prefixes(executable)))
}

fn classify(observed: Result<Option<Installation>, String>) -> Owner {
    match observed {
        Ok(Some(_)) => Owner::HomebrewPackage,
        Ok(None) => Owner::Direct,
        Err(_) => Owner::Unknown,
    }
}

pub(crate) fn require_direct_app(app: &Path) -> Result<(), String> {
    let executable = app.join("Contents").join("MacOS").join("peerbrush");
    require_direct(owner(Some(&executable)))
}

fn require_direct(owner: Owner) -> Result<(), String> {
    match owner {
        Owner::Direct => Ok(()),
        Owner::HomebrewPackage => {
            Err("Homebrew owns this app's updates. The built-in updater cannot replace it.".into())
        }
        _ => Err("App ownership is uncertain; built-in replacement remains unavailable.".into()),
    }
}

pub(super) fn command(executable: &Path) -> Result<Option<PathBuf>, String> {
    let Some(installed) = observe(executable, &prefixes(executable))? else {
        return Ok(None);
    };
    select(installed).map(Some)
}

fn select(installed: Installation) -> Result<PathBuf, String> {
    let target = installed
        .launcher
        .canonicalize()
        .map_err(|_| LAUNCHER_ERROR)?;
    if target != installed.executable
        || !target.metadata().is_ok_and(|metadata| metadata.is_file())
        || installed.launcher.canonicalize().ok().as_ref() != Some(&target)
    {
        return Err(LAUNCHER_ERROR.into());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if target
            .metadata()
            .map_err(|_| LAUNCHER_ERROR)?
            .permissions()
            .mode()
            & 0o111
            == 0
        {
            return Err(LAUNCHER_ERROR.into());
        }
    }
    Ok(installed.launcher)
}

fn prefixes(executable: &Path) -> Vec<PathBuf> {
    candidates(
        executable,
        std::env::var_os("HOMEBREW_PREFIX")
            .as_deref()
            .map(Path::new),
    )
}

fn candidates(executable: &Path, hint: Option<&Path>) -> Vec<PathBuf> {
    // Hints locate candidates only. Receipts and the exact app link must match.
    // shortcut: conventional prefixes plus an inherited prefix/bin launch hint;
    // Finder launches from custom prefixes need that verified location available.
    let mut paths = vec![PathBuf::from("/opt/homebrew"), PathBuf::from("/usr/local")];
    if let Some(hint) = hint {
        paths.push(hint.to_owned());
    }
    if let Some(bin) = executable
        .parent()
        .filter(|path| path.file_name().is_some_and(|name| name == "bin"))
    {
        if let Some(prefix) = bin.parent() {
            paths.push(prefix.to_owned());
        }
    }
    let mut found = Vec::new();
    for path in paths {
        if path.is_absolute()
            && !path
                .components()
                .any(|part| matches!(part, Component::ParentDir))
            && !found.contains(&path)
        {
            found.push(path);
        }
    }
    found
}

fn observe(executable: &Path, prefixes: &[PathBuf]) -> Result<Option<Installation>, String> {
    if !executable.is_absolute()
        || executable
            .components()
            .any(|part| matches!(part, Component::ParentDir))
    {
        return Err(OBSERVATION_ERROR.into());
    }
    let image = executable.canonicalize().map_err(|_| OBSERVATION_ERROR)?;
    let Some(bundle) = image.parent().and_then(Path::parent).and_then(Path::parent) else {
        return Ok(None);
    };
    if bundle
        .file_name()
        .is_none_or(|name| name != "PeerBrush.app")
        || image != bundle.join("Contents").join("MacOS").join("peerbrush")
    {
        return Ok(None);
    }
    let mut found = None;
    for prefix in prefixes {
        let cask = prefix.join("Caskroom").join("peerbrush");
        let record = cask.join(".metadata").join("INSTALL_RECEIPT.json");
        let Some(bytes) = receipt(&record)? else {
            if matches_slot(&cask, env!("CARGO_PKG_VERSION"), bundle)? {
                return Err(OBSERVATION_ERROR.into());
            }
            continue;
        };
        let parsed: Receipt = serde_json::from_slice(&bytes).map_err(|_| OBSERVATION_ERROR)?;
        let version = &parsed.source.version;
        if version.len() > 64
            || !semver::Version::parse(version).is_ok_and(|value| {
                value.pre.is_empty() && value.build.is_empty() && value.to_string() == *version
            })
        {
            return Err(OBSERVATION_ERROR.into());
        }
        if !matches_slot(&cask, version, bundle)? {
            continue;
        }
        if parsed.arch != "arm64"
            || version != env!("CARGO_PKG_VERSION")
            || receipt(&record)?.as_ref() != Some(&bytes)
            || !matches_slot(&cask, version, bundle)?
            || executable.canonicalize().ok().as_ref() != Some(&image)
            || found.is_some()
        {
            return Err(OBSERVATION_ERROR.into());
        }
        found = Some(Installation {
            executable: image.clone(),
            launcher: prefix.join("bin").join("peerbrush"),
        });
    }
    Ok(found)
}

fn matches_slot(cask: &Path, version: &str, bundle: &Path) -> Result<bool, String> {
    let slot = cask.join(version);
    let dmg = matches_app(&slot.join("PeerBrush.app"), bundle)?;
    let zip = matches_app(
        &slot.join(format!("PeerBrush-{version}-macOS-arm64/PeerBrush.app")),
        bundle,
    )?;
    if dmg && zip {
        return Err(OBSERVATION_ERROR.into());
    }
    Ok(dmg || zip)
}

fn matches_app(path: &Path, bundle: &Path) -> Result<bool, String> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(value) => value,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(_) => return Err(OBSERVATION_ERROR.into()),
    };
    if !metadata.file_type().is_symlink() {
        return Ok(false);
    }
    Ok(path.canonicalize().map_err(|_| OBSERVATION_ERROR)? == bundle)
}

fn receipt(path: &Path) -> Result<Option<Vec<u8>>, String> {
    let file = match crate::file_safety::open_native_read(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(OBSERVATION_ERROR.into()),
    };
    let mut bytes = Vec::new();
    file.take(LIMIT + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| OBSERVATION_ERROR)?;
    if bytes.is_empty() || bytes.len() as u64 > LIMIT {
        return Err(OBSERVATION_ERROR.into());
    }
    Ok(Some(bytes))
}

#[cfg(test)]
mod tests;
