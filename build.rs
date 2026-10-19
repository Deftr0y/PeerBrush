use std::{
    env, fs,
    path::{Path, PathBuf},
    process::Command,
};

fn git(root: &Path, args: &[&str]) -> Option<String> {
    let mut command = Command::new("git");
    for (name, _) in env::vars_os().filter(|(name, _)| {
        name.to_string_lossy()
            .to_ascii_uppercase()
            .starts_with("GIT_")
    }) {
        command.env_remove(name);
    }
    let output = command
        .args(["-c", "core.fsmonitor=false"])
        .arg("-C")
        .arg(root)
        .args(args)
        .output()
        .ok()?;
    output.status.success().then_some(())?;
    String::from_utf8(output.stdout).ok()
}

fn watch(path: &Path) {
    if let Some(path) = path
        .to_str()
        .filter(|s| !s.contains('\r') && !s.contains('\n'))
    {
        println!("cargo:rerun-if-changed={path}");
    }
}

fn source_watch_paths(root: &Path, files: &str) -> Vec<PathBuf> {
    let mut paths: Vec<_> = files
        .split('\0')
        .filter(|s| !s.is_empty())
        .map(|s| root.join(s))
        .collect();
    // Watch additions without recursively watching build/runtime output trees.
    for name in [
        "src",
        "tests",
        "examples",
        "integrations",
        "vendor",
        "assets",
    ] {
        let path = root.join(name);
        if path.is_dir() {
            paths.push(path);
        }
    }
    paths
}

fn provenance(root: &Path) -> Option<(String, &'static str)> {
    let top = git(root, &["rev-parse", "--show-toplevel"])?;
    // An extracted source tree may live inside another checkout.
    if fs::canonicalize(top.trim()).ok()? != fs::canonicalize(root).ok()? {
        return None;
    }
    if root.join(".git").is_file() {
        watch(&root.join(".git"));
    }
    for name in ["HEAD", "index", "packed-refs"] {
        let path = git(
            root,
            &["rev-parse", "--path-format=absolute", "--git-path", name],
        )?;
        if Path::new(path.trim()).exists() {
            watch(Path::new(path.trim()));
        }
    }
    if let Some(reference) = git(root, &["symbolic-ref", "-q", "HEAD"]) {
        let path = git(
            root,
            &[
                "rev-parse",
                "--path-format=absolute",
                "--git-path",
                reference.trim(),
            ],
        )?;
        watch(Path::new(path.trim()));
    }
    let files = git(
        root,
        &[
            "ls-files",
            "--cached",
            "--others",
            "--exclude-standard",
            "-z",
        ],
    )?;
    for path in source_watch_paths(root, &files) {
        watch(&path);
    }
    let commit = git(root, &["rev-parse", "HEAD"])?.trim().to_owned();
    if commit.len() != 40
        || !commit
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
    {
        return None;
    }
    let status = git(
        root,
        &["status", "--porcelain=v1", "-z", "--untracked-files=normal"],
    )?;
    let hidden = git(root, &["ls-files", "-v", "-z"])?
        .split('\0')
        .any(|row| {
            row.as_bytes()
                .first()
                .is_some_and(|tag| tag.is_ascii_lowercase() || *tag == b'S')
        });
    Some((
        commit,
        if status.is_empty() && !hidden {
            "clean"
        } else {
            "dirty"
        },
    ))
}

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    // Trusted release tooling computes this invalidator. Never use it as a label.
    println!("cargo:rerun-if-env-changed=PEERBRUSH_BUILD_INPUT_STATE");
    let root = env::var_os("CARGO_MANIFEST_DIR").unwrap();
    let root = Path::new(&root);
    let (commit, tree) = provenance(root).unwrap_or_else(|| ("unknown".into(), "unknown"));
    let version = env::var("CARGO_PKG_VERSION").unwrap();
    let json = format!("{{\"format\":\"PeerBrushBuildInfoV1\",\"schema\":1,\"product\":\"PeerBrush\",\"version\":\"{version}\",\"commit\":\"{commit}\",\"tree\":\"{tree}\",\"end\":\"PeerBrushBuildInfoEnd\"}}");
    fs::write(
        Path::new(&env::var_os("OUT_DIR").unwrap()).join("build-info.json"),
        json,
    )
    .unwrap();
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn source_labels_follow_git_state_and_reject_archive_parent_repositories() {
        let directory = env::temp_dir().join(format!(
            "peerbrush-build-info-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir(&directory).unwrap();
        assert!(provenance(&directory).is_none());
        git(&directory, &["init", "-q"]).unwrap();
        fs::write(directory.join("input.rs"), "original").unwrap();
        git(&directory, &["add", "input.rs"]).unwrap();
        let commit = [
            "-c",
            "commit.gpgsign=false",
            "-c",
            "user.name=Build Info Tests",
            "-c",
            "user.email=build-info-tests@example.invalid",
            "commit",
            "-qm",
            "Synthetic fixture",
        ];
        git(&directory, &commit).unwrap();
        let (first, state) = provenance(&directory).unwrap();
        assert_eq!(state, "clean");
        fs::write(directory.join("input.rs"), "changed").unwrap();
        assert_eq!(provenance(&directory), Some((first.clone(), "dirty")));
        git(&directory, &["add", "input.rs"]).unwrap();
        git(&directory, &commit).unwrap();
        let (second, state) = provenance(&directory).unwrap();
        assert_eq!(state, "clean");
        assert_ne!(first, second);
        for (flag, clear) in [
            ("--assume-unchanged", "--no-assume-unchanged"),
            ("--skip-worktree", "--no-skip-worktree"),
        ] {
            git(&directory, &["update-index", flag, "input.rs"]).unwrap();
            fs::write(directory.join("input.rs"), "hidden changed bytes").unwrap();
            assert!(git(&directory, &["status", "--porcelain"])
                .unwrap()
                .is_empty());
            assert_eq!(provenance(&directory), Some((second.clone(), "dirty")));
            git(&directory, &["update-index", clear, "input.rs"]).unwrap();
            fs::write(directory.join("input.rs"), "changed").unwrap();
        }
        fs::create_dir(directory.join("src")).unwrap();
        fs::create_dir(directory.join("target")).unwrap();
        fs::create_dir(directory.join(".runtime")).unwrap();
        fs::write(directory.join(".gitignore"), "target/\n.runtime/\n").unwrap();
        git(&directory, &["add", ".gitignore"]).unwrap();
        git(&directory, &commit).unwrap();
        let files = git(&directory, &["ls-files", "-z"]).unwrap();
        let watches = source_watch_paths(&directory, &files);
        let added = directory.join("src/new-input.rs");
        assert!(watches.iter().any(|p| p.is_dir() && added.starts_with(p)));
        assert!(watches.contains(&directory.join(".gitignore")));
        assert!(!watches.contains(&directory));
        assert!(!watches.contains(&directory.join("target")));
        assert!(!watches.contains(&directory.join(".runtime")));
        fs::write(added, "new compiler input").unwrap();
        assert_eq!(provenance(&directory).unwrap().1, "dirty");
        let extracted = directory.join("extracted-source");
        fs::create_dir(&extracted).unwrap();
        assert!(provenance(&extracted).is_none());
        fs::remove_dir_all(directory).unwrap();
    }
}
