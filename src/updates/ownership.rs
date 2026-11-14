//! Conservative process-path classification, without invoking a package manager.
#[cfg(any(target_os = "linux", test))]
use super::Owner;
use std::path::{Component, Path};

#[cfg(any(target_os = "linux", test))]
pub(super) fn linux(executable: Option<&Path>, flatpak: bool) -> Owner {
    if flatpak {
        return Owner::LinuxPackage;
    }
    let Some(executable) = executable else {
        return Owner::Unknown;
    };
    if nix_store(executable) {
        Owner::NixPackage
    } else if executable == Path::new("/usr/bin/peerbrush")
        || executable.starts_with("/usr/lib/peerbrush")
        || executable.starts_with("/opt/peerbrush")
    {
        Owner::LinuxPackage
    } else {
        Owner::Direct
    }
}

pub(crate) fn nix_store(executable: &Path) -> bool {
    // shortcut: default /nix/store; custom stores need an explicit observed root.
    // current_exe observes the process image behind profile/wrapper symlinks.
    // Reject lookalike prefixes and lexical escapes from the immutable object.
    let Ok(relative) = executable.strip_prefix("/nix/store") else {
        return false;
    };
    let mut parts = relative.components();
    let Some(Component::Normal(object)) = parts.next() else {
        return false;
    };
    let Some((digest, name)) = object.to_str().and_then(|name| name.split_once('-')) else {
        return false;
    };
    digest.len() == 32
        && digest
            .bytes()
            .all(|byte| b"0123456789abcdfghijklmnpqrsvwxyz".contains(&byte))
        && !name.is_empty()
        && parts.all(|part| matches!(part, Component::Normal(_)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nix_process_images_stay_with_nix_across_versions_and_wrapped_launches() {
        for object in [
            "00000000000000000000000000000000-peerbrush-0.30.1",
            "123456789abcdfghijklmnpqrsvwxyz0-peerbrush-custom",
        ] {
            for binary in [
                "bin/peerbrush",
                "bin/.peerbrush-wrapped",
                "libexec/peerbrush",
            ] {
                let path = format!("/nix/store/{object}/{binary}");
                assert_eq!(linux(Some(Path::new(&path)), false), Owner::NixPackage);
            }
        }
        assert!(Owner::NixPackage.description().contains("rollbacks"));
        assert_ne!(Owner::NixPackage, Owner::Direct);
    }

    #[test]
    fn unrelated_paths_and_lexical_escapes_do_not_claim_a_nix_installation() {
        for path in [
            "/tmp/nix/store/00000000000000000000000000000000-peerbrush/bin/peerbrush",
            "/nix/store-other/00000000000000000000000000000000-peerbrush/bin/peerbrush",
            "nix/store/00000000000000000000000000000000-peerbrush/bin/peerbrush",
            "/nix/store/00000000000000000000000000000000-peerbrush/../local/bin/peerbrush",
            "/nix/store/not-a-store-digest-peerbrush/bin/peerbrush",
            "/nix/store/eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee-peerbrush/bin/peerbrush",
            "/nix/store/00000000000000000000000000000000-/bin/peerbrush",
            "/nix/store/bin/peerbrush",
            "/nix/store",
        ] {
            assert_eq!(linux(Some(Path::new(path)), false), Owner::Direct, "{path}");
        }
    }

    #[test]
    fn existing_linux_routes_and_unknown_process_observations_keep_their_owner() {
        for path in [
            "/usr/bin/peerbrush",
            "/usr/lib/peerbrush/peerbrush",
            "/opt/peerbrush/bin/peerbrush",
        ] {
            assert_eq!(linux(Some(Path::new(path)), false), Owner::LinuxPackage);
        }
        assert_eq!(linux(None, false), Owner::Unknown);
        assert_eq!(linux(None, true), Owner::LinuxPackage);
        assert_eq!(
            linux(Some(Path::new("/tmp/peerbrush")), false),
            Owner::Direct
        );
    }
}
