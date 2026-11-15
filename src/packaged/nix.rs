//! Keep MCP registration on a Nix profile link that follows package upgrades.
use std::{
    ffi::OsStr,
    fs,
    path::{Component, Path, PathBuf},
};

pub(super) fn command(
    executable: &Path,
    home: Option<&Path>,
    state: Option<&Path>,
    user: Option<&OsStr>,
    launch: Option<&Path>,
    search: Option<&OsStr>,
    data_dirs: Option<&OsStr>,
) -> Result<PathBuf, String> {
    if let Some(launcher) = launch
        .filter(|path| path.file_name() == Some(OsStr::new("peerbrush")))
        .and_then(Path::parent)
        .and_then(|bin| verified_custom_launcher(executable, bin))
    {
        return Ok(launcher);
    }
    let candidates = profiles(home, state, user);
    match select(executable, &candidates) {
        Ok(launcher) => Ok(launcher),
        Err(reason) => custom_launcher(executable, search, data_dirs)?.ok_or(reason),
    }
}

fn profiles(home: Option<&Path>, state: Option<&Path>, user: Option<&OsStr>) -> Vec<PathBuf> {
    let home = home.filter(|path| path.is_absolute());
    let state = state
        .filter(|path| path.is_absolute())
        .map(Path::to_owned)
        .or_else(|| home.map(|path| path.join(".local/state")));
    let mut candidates = Vec::new();
    if let Some(state) = &state {
        candidates.push(state.join("nix/profile/bin/peerbrush"));
    }
    if let Some(home) = home {
        candidates.push(home.join(".nix-profile/bin/peerbrush"));
    }
    if let Some(state) = state {
        candidates.push(state.join("nix/profiles/profile/bin/peerbrush"));
    }
    if let Some(user) = user {
        let mut parts = Path::new(user).components();
        if matches!(parts.next(), Some(Component::Normal(name)) if name == user) && parts.next().is_none() {
            candidates.push(
                Path::new("/etc/profiles/per-user")
                    .join(user)
                    .join("bin/peerbrush"),
            );
            candidates.push(
                Path::new("/nix/var/nix/profiles/per-user")
                    .join(user)
                    .join("profile/bin/peerbrush"),
            );
        }
    }
    candidates.push(PathBuf::from("/run/current-system/sw/bin/peerbrush"));
    candidates.push(PathBuf::from("/nix/var/nix/profiles/default/bin/peerbrush"));
    candidates
}

fn profile_store(profile: &Path) -> Option<PathBuf> {
    let link = fs::read_link(profile).ok()?;
    let name = link.file_name()?;
    let generation = profile.with_file_name(name);
    if link != Path::new(name) && link != generation {
        return None;
    }
    let number = name
        .as_encoded_bytes()
        .strip_prefix(profile.file_name()?.as_encoded_bytes())?
        .strip_prefix(b"-")?
        .strip_suffix(b"-link")?;
    if !number
        .first()
        .is_some_and(|byte| (b'1'..=b'9').contains(byte))
        || !number.iter().all(u8::is_ascii_digit)
    {
        return None;
    }
    let store = fs::read_link(generation).ok()?;
    (store.parent() == Some(Path::new("/nix/store"))
        && crate::updates::ownership::nix_store(&store))
    .then_some(store)
}

fn custom_launcher(
    executable: &Path,
    search: Option<&OsStr>,
    data_dirs: Option<&OsStr>,
) -> Result<Option<PathBuf>, String> {
    let bounded = |value: &&OsStr| value.as_encoded_bytes().len() <= 32 * 1024;
    let bins = search.filter(bounded).into_iter().flat_map(|value| std::env::split_paths(value));
    let desktop_bins = data_dirs.filter(bounded).into_iter()
        .flat_map(|value| std::env::split_paths(value))
        .filter(|share| share.file_name() == Some(OsStr::new("share")))
        .map(|share| share.with_file_name("bin"));
    let mut selected = None;
    for bin in bins.chain(desktop_bins) {
        let Some(candidate) = verified_custom_launcher(executable, &bin) else {
            continue;
        };
        if selected.as_ref().is_some_and(|old| old != &candidate) {
            return Err("Multiple custom Nix profiles match this running build. Launch PeerBrush through the intended profile's bin directory and retry Connect AI.".into());
        }
        selected = Some(candidate);
    }
    Ok(selected)
}

fn verified_custom_launcher(executable: &Path, bin: &Path) -> Option<PathBuf> {
    if !bin.is_absolute()
        || bin.file_name() != Some(OsStr::new("bin"))
        || bin.components().any(|part| part == Component::ParentDir)
        || crate::updates::ownership::nix_store(bin)
    {
        return None;
    }
    let profile = bin.parent()?;
    let store = profile_store(profile)?;
    // A Nix generation must point to a profile tree, not a package output.
    if !fs::symlink_metadata(&store).ok()?.is_dir()
        || !fs::symlink_metadata(store.join("manifest.json"))
            .ok()?
            .is_file()
    {
        return None;
    }
    let candidate = select(executable, &[bin.join("peerbrush")]).ok()?;
    (profile_store(profile).as_ref() == Some(&store)).then_some(candidate)
}

fn select(executable: &Path, candidates: &[PathBuf]) -> Result<PathBuf, String> {
    let executable = executable
        .canonicalize()
        .map_err(|_| "Cannot observe this Nix executable for MCP registration")?;
    for candidate in candidates {
        if !candidate.is_absolute() || crate::updates::ownership::nix_store(candidate) {
            continue;
        }
        let Some(profile) = candidate.parent().and_then(Path::parent) else {
            continue;
        };
        let Ok(profile_target) = profile.canonicalize() else {
            continue;
        };
        let Ok(target) = candidate.canonicalize() else {
            continue;
        };
        // A conventional profile path can itself be an alias to a package output.
        if target.parent().and_then(Path::parent) == Some(profile_target.as_path()) {
            continue;
        }
        let Ok(metadata) = target.metadata() else {
            continue;
        };
        if !metadata.is_file() {
            continue;
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if metadata.permissions().mode() & 0o111 == 0 {
                continue;
            }
        }
        // makeWrapper keeps the native image beside its public launcher, in
        // the same immutable store object. Never interpret arbitrary scripts.
        let wrapper = executable
            .file_name()
            .is_some_and(|name| name == ".peerbrush-wrapped")
            && target.file_name().is_some_and(|name| name == "peerbrush")
            && executable.parent() == target.parent();
        if target == executable || wrapper {
            // Profile updates can happen concurrently with the observation.
            if candidate.canonicalize().ok().as_ref() == Some(&target)
                && profile.canonicalize().ok().as_ref() == Some(&profile_target)
            {
                return Ok(candidate.clone());
            }
        }
    }
    Err("No stable Nix MCP launcher matches this running build. Open PeerBrush from your profile or make its bin or share directory available, then retry Connect AI.".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profile_candidates_preserve_native_paths_and_ignore_relative_environment_values() {
        let home = std::env::temp_dir().join("Nix profile user space");
        let state = home.join("custom state");
        let candidates = profiles(Some(&home), Some(&state), None);
        assert_eq!(candidates[0], state.join("nix/profile/bin/peerbrush"));
        assert_eq!(candidates[1], home.join(".nix-profile/bin/peerbrush"));
        assert_eq!(
            candidates[2],
            state.join("nix/profiles/profile/bin/peerbrush")
        );
        assert_eq!(
            profiles(Some(&home), Some(Path::new("relative")), None)[0],
            home.join(".local/state/nix/profile/bin/peerbrush")
        );
        let defaults = profiles(Some(Path::new("relative")), None, None);
        assert_eq!(defaults.len(), 2);
        for invalid in ["../escape", "profile-user/", "profile-user/."] {
            assert_eq!(profiles(None, None, Some(OsStr::new(invalid))), defaults);
        }
        let candidates = profiles(None, None, Some(OsStr::new("profile-user")));
        assert_eq!(candidates[0], Path::new("/etc/profiles/per-user/profile-user/bin/peerbrush"));
        assert_eq!(candidates[1], Path::new("/nix/var/nix/profiles/per-user/profile-user/profile/bin/peerbrush"));
    }

    #[cfg(unix)]
    #[test]
    fn custom_profile_requires_its_own_numbered_link_to_a_store_object() {
        use std::os::unix::fs::symlink;

        let root = std::env::temp_dir().join(format!("pb-custom-nix-{}", crate::engine::id()));
        fs::create_dir(&root).unwrap();
        let profile = root.join("Creative 工具");
        let first = root.join("Creative 工具-1-link");
        let second = root.join("Creative 工具-2-link");
        let old = Path::new("/nix/store/00000000000000000000000000000000-profile");
        let new = Path::new("/nix/store/11111111111111111111111111111111-profile");
        symlink(old, &first).unwrap();
        symlink(new, &second).unwrap();
        symlink(first.file_name().unwrap(), &profile).unwrap();
        assert_eq!(profile_store(&profile).as_deref(), Some(old));
        assert!(
            profile_store(&first).is_none(),
            "generation links are version-pinned"
        );
        fs::remove_file(&profile).unwrap();
        symlink(&second, &profile).unwrap();
        assert_eq!(profile_store(&profile).as_deref(), Some(new));
        fs::remove_file(&second).unwrap();
        symlink(new.join("bin"), &second).unwrap();
        assert!(profile_store(&profile).is_none());
        fs::remove_file(&profile).unwrap();
        symlink(old, &profile).unwrap();
        assert!(
            profile_store(&profile).is_none(),
            "direct store links are version-pinned"
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn profile_link_follows_upgrades_while_wrong_targets_and_other_wrappers_are_refused() {
        use std::{
            fs,
            os::unix::fs::{symlink, PermissionsExt},
        };
        let root = std::env::temp_dir().join(format!("pb-nix-mcp-{}", crate::engine::id()));
        let state = root.join("state");
        let profile = state.join("nix/profiles/profile/bin/peerbrush");
        fs::create_dir_all(profile.parent().unwrap()).unwrap();
        let mut versions = Vec::new();
        for version in ["old", "new"] {
            let bin = root.join(version).join("bin");
            fs::create_dir_all(&bin).unwrap();
            let executable = bin.join("peerbrush");
            fs::write(&executable, b"native image fixture; never executed").unwrap();
            fs::set_permissions(&executable, fs::Permissions::from_mode(0o755)).unwrap();
            versions.push(executable);
        }
        symlink(&versions[0], &profile).unwrap();
        let pinned = root.join(".nix-profile");
        symlink(root.join("old"), &pinned).unwrap();
        assert!(select(&versions[0], &[pinned.join("bin/peerbrush")]).is_err());
        assert_eq!(
            command(
                &versions[0],
                Some(&root),
                Some(&state),
                None,
                None,
                None,
                None,
            )
            .unwrap(),
            profile
        );
        assert!(select(&versions[1], &[profile.clone()]).is_err());
        fs::remove_file(&profile).unwrap();
        symlink(&versions[1], &profile).unwrap();
        assert_eq!(
            command(&versions[1], None, Some(&state), None, None, None, None).unwrap(),
            profile
        );
        assert!(select(&versions[0], &[profile.clone()]).is_err());
        let wrapped = versions[1].with_file_name(".peerbrush-wrapped");
        fs::write(&wrapped, b"wrapped native image fixture").unwrap();
        fs::set_permissions(&wrapped, fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(select(&wrapped, &[profile.clone()]).unwrap(), profile);
        let unrelated = wrapped.with_file_name("different-wrapper");
        fs::write(&unrelated, b"unrecognized native image").unwrap();
        assert!(select(&unrelated, &[profile.clone()]).is_err());
        fs::set_permissions(&versions[1], fs::Permissions::from_mode(0o644)).unwrap();
        assert!(select(&wrapped, &[profile.clone()]).is_err());
        fs::remove_file(&profile).unwrap();
        assert!(select(&wrapped, &[profile]).is_err());
        fs::remove_dir_all(root).unwrap();
    }
}
