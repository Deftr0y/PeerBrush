//! Observe Scoop's local receipts; retain its upgrade-stable MCP launcher.
use crate::updates::Owner;
use serde::Deserialize;
use std::{
    io::Read,
    path::{Component, Path, PathBuf},
};

const RECEIPT_LIMIT: u64 = 128 * 1024;
const OBSERVATION_ERROR: &str = "Cannot observe this Scoop installation's local records. No package trust or update availability was established.";
const LAUNCHER_ERROR: &str = "Scoop's current PeerBrush launcher does not match this running build. Open the version selected by Scoop, then retry Connect AI; client settings were not changed.";

#[derive(Deserialize)]
struct Manifest {
    version: String,
    bin: String,
}
#[derive(Deserialize)]
struct Install {
    architecture: String,
}
struct Installation {
    executable: PathBuf,
    launcher: PathBuf,
}

pub(crate) fn owner(executable: Option<&Path>) -> Owner {
    let Some(executable) = executable else {
        return Owner::Unknown;
    };
    match observe(executable) {
        Ok(Some(_)) => Owner::ScoopPackage,
        Ok(None) => Owner::Direct,
        Err(_) => Owner::Unknown,
    }
}

pub(super) fn command(executable: &Path) -> Result<Option<PathBuf>, String> {
    let Some(installed) = observe(executable)? else {
        return Ok(None);
    };
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
    Ok(Some(installed.launcher))
}

fn observe(executable: &Path) -> Result<Option<Installation>, String> {
    if !executable.is_absolute()
        || executable
            .components()
            .any(|part| matches!(part, Component::ParentDir))
    {
        return Err(OBSERVATION_ERROR.into());
    }
    let image = executable.canonicalize().map_err(|_| OBSERVATION_ERROR)?;
    let Some((directory, app, version)) = layout(&image) else {
        return Ok(None);
    };
    // shortcut: the generated x64 manifest's flat stable-version/current layout.
    // Other architectures/layouts require their own native package acceptance.
    let manifest_bytes = receipt(directory, "scoop-manifest.json", "manifest.json")?;
    let install_bytes = receipt(directory, "scoop-install.json", "install.json")?;
    let manifest: Manifest =
        serde_json::from_slice(without_bom(&manifest_bytes)).map_err(|_| OBSERVATION_ERROR)?;
    let install: Install =
        serde_json::from_slice(without_bom(&install_bytes)).map_err(|_| OBSERVATION_ERROR)?;
    if manifest.version != version
        || manifest.version != env!("CARGO_PKG_VERSION")
        || !manifest.bin.eq_ignore_ascii_case("peerbrush.exe")
        || install.architecture != "64bit"
        || receipt(directory, "scoop-manifest.json", "manifest.json")? != manifest_bytes
        || receipt(directory, "scoop-install.json", "install.json")? != install_bytes
        || executable.canonicalize().ok().as_ref() != Some(&image)
    {
        return Err(OBSERVATION_ERROR.into());
    }
    Ok(Some(Installation {
        executable: image.clone(),
        launcher: app.join("current").join("peerbrush.exe"),
    }))
}

fn layout(executable: &Path) -> Option<(&Path, &Path, &str)> {
    let name = executable.file_name()?.to_str()?;
    let directory = executable.parent()?;
    let app = directory.parent()?;
    let apps = app.parent()?;
    if !name.eq_ignore_ascii_case("peerbrush.exe")
        || !app.file_name()?.to_str()?.eq_ignore_ascii_case("peerbrush")
        || !apps.file_name()?.to_str()?.eq_ignore_ascii_case("apps")
    {
        return None;
    }
    let version = directory.file_name()?.to_str()?;
    let parsed = (version.len() <= 64)
        .then(|| semver::Version::parse(version).ok())
        .flatten()?;
    (parsed.pre.is_empty() && parsed.build.is_empty()).then_some((directory, app, version))
}

fn without_bom(bytes: &[u8]) -> &[u8] {
    bytes.strip_prefix(b"\xef\xbb\xbf").unwrap_or(bytes)
}

fn receipt(directory: &Path, preferred: &str, legacy: &str) -> Result<Vec<u8>, String> {
    let file = match crate::file_safety::open_native_read(&directory.join(preferred)) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            crate::file_safety::open_native_read(&directory.join(legacy))
                .map_err(|_| OBSERVATION_ERROR)?
        }
        Err(_) => return Err(OBSERVATION_ERROR.into()),
    };
    let mut bytes = Vec::new();
    file.take(RECEIPT_LIMIT + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| OBSERVATION_ERROR)?;
    if bytes.is_empty() || bytes.len() as u64 > RECEIPT_LIMIT {
        return Err(OBSERVATION_ERROR.into());
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests;
