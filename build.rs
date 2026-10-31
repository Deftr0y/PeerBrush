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

fn windows_version_resource(version: &str, debug: bool) -> Result<String, &'static str> {
    if version.len() > 128
        || !version
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b".+-".contains(&b))
    {
        return Err("Windows version metadata requires bounded Cargo SemVer text");
    }
    let numeric = version.split(['-', '+']).next().unwrap();
    let parts = numeric
        .split('.')
        .map(str::parse::<u16>)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| "Windows version components must fit u16")?;
    if parts.len() != 3 {
        return Err("Windows version metadata requires three numeric components");
    }
    let fixed = format!("{},{},{},0", parts[0], parts[1], parts[2]);
    let flags = u8::from(debug)
        | if version.split('+').next().unwrap().contains('-') {
            2
        } else {
            0
        };
    Ok(include_str!("packaging/windows/version.rc.in")
        .replace("@FIXED_VERSION@", &fixed)
        .replace("@FLAGS@", &flags.to_string())
        .replace("@VERSION@", version))
}

fn windows_resource_tools(
    bin: &Path,
    version: &str,
    host: &str,
) -> Result<[PathBuf; 2], &'static str> {
    if !["10.0.18362.0", "10.0.26100.0"].contains(&version)
        || !bin.is_absolute()
        || bin.file_name().and_then(|s| s.to_str()) != Some(version)
        || host != "x64"
    {
        return Err("Use an initialized qualified Windows SDK resource compiler (18362 or 26100)");
    }
    Ok([
        bin.join(host).join("rc.exe"),
        bin.join(host).join("rcdll.dll"),
    ])
}

fn link_windows_version(root: &Path, version: &str) {
    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    assert_eq!(
        env::var("CARGO_CFG_TARGET_ENV").as_deref(),
        Ok("msvc"),
        "Windows version resources require the supported MSVC target"
    );
    for key in [
        "WINDOWSSDKVERBINPATH",
        "WINDOWSSDKVERSION",
        "VSCMD_ARG_HOST_ARCH",
        "DEBUG",
    ] {
        println!("cargo:rerun-if-env-changed={key}");
    }
    watch(&root.join("packaging/windows/version.rc.in"));
    let bin = PathBuf::from(
        env::var_os("WINDOWSSDKVERBINPATH")
            .expect("Initialize the Windows SDK/MSVC environment before Cargo"),
    );
    let sdk = env::var("WINDOWSSDKVERSION").expect("Missing initialized Windows SDK version");
    let sdk = sdk.trim_end_matches(['\\', '/']);
    let host = env::var("VSCMD_ARG_HOST_ARCH").expect("Missing initialized MSVC host architecture");
    let [compiler, support] = windows_resource_tools(&bin, sdk, &host).unwrap();
    for path in [&compiler, &support] {
        assert!(
            path.is_file() && !path.is_symlink(),
            "Selected Windows SDK resource tool is missing or linked"
        );
        watch(path);
    }
    let output = PathBuf::from(env::var_os("OUT_DIR").unwrap());
    let mut binaries = vec!["peerbrush"];
    if env::var_os("CARGO_FEATURE_WINDOWS_SETUP").is_some() {
        binaries.push("peerbrush-setup");
    }
    for binary in binaries {
        let script = output.join(format!("{binary}-version.rc"));
        let resource = output.join(format!("{binary}-version.res"));
        let mut source =
            windows_version_resource(version, env::var("DEBUG").as_deref() == Ok("true")).unwrap();
        if binary == "peerbrush-setup" {
            source = source
                .replace("PeerBrush graphics editor", "PeerBrush installer")
                .replace("\"peerbrush\\0\"", "\"peerbrush-setup\\0\"")
                .replace("peerbrush.exe", "peerbrush-setup.exe");
        }
        fs::write(&script, source).unwrap();
        if resource.exists() {
            assert!(
                resource.is_file() && !resource.is_symlink(),
                "Unexpected prior resource output"
            );
            fs::remove_file(&resource).unwrap();
        }
        let status = Command::new(&compiler)
            .args(["/nologo", "/c65001", "/l0x0409", "/fo"])
            .arg(&resource)
            .arg(&script)
            .status()
            .expect("Windows SDK resource compiler could not start");
        assert!(
            status.success(),
            "Windows version resource compilation failed"
        );
        let metadata = fs::symlink_metadata(&resource)
            .expect("Windows resource compiler produced no resource");
        assert!(
            metadata.is_file() && metadata.len() > 0 && metadata.len() <= 65536,
            "Windows resource output is invalid or exceeds its bound"
        );
        println!("cargo:rustc-link-arg-bin={binary}={}", resource.display());
    }
}

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    // Trusted release tooling computes this invalidator. Never use it as a label.
    println!("cargo:rerun-if-env-changed=PEERBRUSH_BUILD_INPUT_STATE");
    let root = env::var_os("CARGO_MANIFEST_DIR").unwrap();
    let root = Path::new(&root);
    let (commit, tree) = provenance(root).unwrap_or_else(|| ("unknown".into(), "unknown"));
    let version = env::var("CARGO_PKG_VERSION").unwrap();
    link_windows_version(root, &version);
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
    fn windows_resource_versions_flags_and_sdk_selection_are_explicit() {
        let stable = windows_version_resource("0.30.0", false).unwrap();
        assert!(stable.contains("FILEVERSION 0,30,0,0"));
        assert!(stable.contains("PRODUCTVERSION 0,30,0,0"));
        assert!(stable.contains("FILEFLAGS 0L"));
        assert!(stable.contains("\"ProductVersion\", \"0.30.0\\0\""));
        let pre = windows_version_resource("0.30.0-dev.1+source", true).unwrap();
        assert!(pre.contains("FILEFLAGS 3L"));
        assert!(pre.contains("\"FileVersion\", \"0.30.0-dev.1+source\\0\""));
        for invalid in ["65536.0.0", "0.30", "0.30.0\"", "0.30.0\n"] {
            assert!(windows_version_resource(invalid, false).is_err());
        }
        let base = env::temp_dir().join("10.0.26100.0");
        assert_eq!(
            windows_resource_tools(&base, "10.0.26100.0", "x64").unwrap()[0],
            base.join("x64/rc.exe")
        );
        assert!(windows_resource_tools(&base, "10.0.18362.0", "x64").is_err());
        assert!(
            windows_resource_tools(Path::new("relative/10.0.26100.0"), "10.0.26100.0", "x64")
                .is_err()
        );
        assert!(windows_resource_tools(&base, "10.0.26100.0", "unknown").is_err());
    }
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
