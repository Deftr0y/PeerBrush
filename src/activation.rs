//! Native file activation is queued for the same new-project UI used by File > Open.
//! The authenticated application RPC never replaces a document to service a launch.
use crate::{server::Shared, workspace};
use std::{
    collections::VecDeque,
    ffi::OsString,
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
};

#[cfg(target_os = "macos")]
pub mod macos;

pub const MAX_PENDING_FILES: usize = workspace::MAX_PROJECTS;

/// Extract global options without consuming a filename passed after --open or --.
pub fn arguments(
    args: impl IntoIterator<Item = OsString>,
    default_state_dir: PathBuf,
) -> Result<(PathBuf, Vec<OsString>), String> {
    storage_arguments(args, default_state_dir).map(|(state, args, _)| (state, args))
}

/// An explicit workspace keeps its existing instance-local preferences.
pub fn storage_arguments(
    args: impl IntoIterator<Item = OsString>,
    default_state_dir: PathBuf,
) -> Result<(PathBuf, Vec<OsString>, bool), String> {
    let mut state_dir = default_state_dir;
    let mut local_preferences = false;
    let mut rest = Vec::new();
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        if arg == "--" {
            rest.push(arg);
            rest.extend(args);
            break;
        }
        if arg == "--open" {
            rest.push(arg);
            rest.push(args.next().ok_or("--open needs a file path")?);
        } else if arg == "--state-dir" {
            let path = args.next().ok_or("--state-dir needs a path")?;
            if path.is_empty() {
                return Err("--state-dir needs a path".into());
            }
            state_dir = PathBuf::from(path);
            local_preferences = true;
        } else {
            rest.push(arg);
        }
    }
    Ok((state_dir, rest, local_preferences))
}

/// Classify error presentation before parsing can fail. Broken MCP/headless
/// arguments must still exit cleanly without blocking on a GUI error dialog.
pub fn native_error_dialog(args: impl IntoIterator<Item = OsString>) -> bool {
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        match arg.to_str() {
            Some("--state-dir") => {
                args.next();
            }
            Some(
                "mcp" | "cli" | "discover" | "settings" | "updates" | "--build-info" | "--version"
                | "-V" | "--help" | "--headless",
            ) => return false,
            _ => return true,
        }
    }
    true
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Launch {
    pub files: Vec<PathBuf>,
    pub headless: bool,
}
impl Launch {
    pub fn parse(args: impl IntoIterator<Item = OsString>) -> Result<Self, String> {
        let mut launch = Self::default();
        let mut args = args.into_iter();
        while let Some(arg) = args.next() {
            if arg == "--" {
                launch.files.extend(args.map(PathBuf::from));
                break;
            } else if arg == "--open" {
                // One OS argument, never shell-split or converted lossily to text.
                launch.files.push(PathBuf::from(
                    args.next().ok_or("--open needs a file path")?,
                ));
            } else if arg == "--headless" {
                launch.headless = true;
            } else if arg.to_string_lossy().starts_with('-') {
                return Err(format!(
                    "Unknown option: {}. Use --open PATH to open a file.",
                    arg.to_string_lossy()
                ));
            } else {
                launch.files.push(PathBuf::from(arg));
            }
        }
        if launch.files.iter().any(|p| p.as_os_str().is_empty()) {
            return Err("Choose a nonempty file path".into());
        }
        if launch.files.len() > MAX_PENDING_FILES {
            return Err(format!("Open at most {MAX_PENDING_FILES} files at a time"));
        }
        if launch.headless && !launch.files.is_empty() {
            return Err("File activation needs the native window; omit --headless".into());
        }
        Ok(launch)
    }

    /// Resolve against the launching process, not the existing window's directory.
    pub fn absolute_files(&self) -> Result<Vec<PathBuf>, String> {
        self.files
            .iter()
            .map(|path| std::path::absolute(path).map_err(|e| e.to_string()))
            .collect()
    }
}

#[derive(Default)]
pub(crate) struct Queue {
    enabled: bool,
    files: VecDeque<PathBuf>,
    quit: Arc<AtomicBool>,
}

/// Native callbacks retain only this presentation signal, never an engine lock.
pub(crate) fn quit_signal(registry: &workspace::Registry) -> Arc<AtomicBool> {
    registry.lock().unwrap().activation.quit.clone()
}

/// Consume one coalesced native Quit request through the ordinary guarded UI.
pub fn take_quit(registry: &workspace::Registry) -> bool {
    let workspace = registry.lock().unwrap();
    workspace.activation.enabled
        && !workspace.exiting
        && workspace.activation.quit.swap(false, Ordering::AcqRel)
}

/// Enable before publishing the listener so simultaneous launches can queue safely.
pub fn enable(root: &Shared) {
    workspace::attach(root).lock().unwrap().activation.enabled = true;
}

pub fn enqueue(root: &Shared, files: Vec<PathBuf>) -> Result<usize, String> {
    if files.is_empty() || files.len() > MAX_PENDING_FILES {
        return Err(format!(
            "Choose between 1 and {MAX_PENDING_FILES} files to open"
        ));
    }
    if files.iter().any(|path| !path.is_absolute()) {
        return Err("File activation needs absolute file paths".into());
    }
    let count = files.len();
    let registry = workspace::attach(root);
    {
        let mut workspace = registry.lock().unwrap();
        if !workspace.activation.enabled {
            return Err("This PeerBrush instance has no native window. Use a different --state-dir to open files in a window.".into());
        }
        if workspace.exiting {
            return Err("PeerBrush is exiting; open the files again after it closes".into());
        }
        if workspace.activation.files.len() + count > MAX_PENDING_FILES {
            return Err(
                "Too many files are waiting to open. Finish the current opening, then try again."
                    .into(),
            );
        }
        workspace.activation.files.extend(files);
    }
    root.lock().unwrap().focus_requested = true;
    Ok(count)
}

pub(crate) fn next(registry: &workspace::Registry) -> Option<PathBuf> {
    let mut workspace = registry.lock().unwrap();
    if workspace.exiting {
        return None;
    }
    workspace.activation.files.pop_front()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{engine::Engine, server};
    use serde_json::json;
    use std::sync::{Arc, Mutex};

    fn args(values: &[&str]) -> Vec<OsString> {
        values.iter().map(OsString::from).collect()
    }

    #[test]
    fn native_quit_coalesces_without_locks_or_file_queue_changes() {
        for depth in [8, 16] {
            let mut engine = Engine::new();
            engine.doc = crate::engine::Document::new_depth(64, 64, depth).unwrap();
            engine.doc.revision = 7;
            let root = Arc::new(Mutex::new(engine));
            let registry = workspace::attach(&root);
            let signal = quit_signal(&registry);
            signal.store(true, Ordering::Release);
            assert!(
                !take_quit(&registry),
                "headless instances never process native Quit"
            );
            enable(&root);
            let file = std::env::temp_dir().join("synthetic pending.psd");
            enqueue(&root, vec![file.clone()]).unwrap();
            let before = root.lock().unwrap().doc.clone();
            {
                let _engine = root.lock().unwrap();
                let _workspace = registry.lock().unwrap();
                signal.store(true, Ordering::Release);
                signal.store(true, Ordering::Release);
            }
            assert!(take_quit(&registry));
            assert!(!take_quit(&registry));
            assert_eq!(next(&registry), Some(file));
            assert_eq!(
                serde_json::to_value(&root.lock().unwrap().doc).unwrap(),
                serde_json::to_value(&before).unwrap()
            );
            registry.lock().unwrap().exiting = true;
            signal.store(true, Ordering::Release);
            assert!(!take_quit(&registry));
        }
    }

    #[test]
    fn malformed_noninteractive_arguments_never_request_error_dialogs() {
        for command in [
            vec!["mcp", "--state-dir"],
            vec!["mcp", "--state-dir", ""],
            vec!["--headless", "--state-dir", ""],
            vec!["--state-dir", "", "cli"],
            vec!["--state-dir", "workspace", "mcp"],
            vec!["--build-info", "--state-dir"],
        ] {
            assert!(!native_error_dialog(args(&command)));
        }
        assert!(native_error_dialog(args(&["--open", "mcp"])));
        assert!(native_error_dialog(args(&["--", "--headless"])));
        assert!(native_error_dialog(args(&["--open"])));
    }

    #[test]
    fn paths_keep_spaces_unicode_and_command_names() {
        let launch = Launch::parse(args(&[
            "--open",
            "作品/my picture.psd",
            "--open",
            "mcp",
            "other.psb",
        ]))
        .unwrap();
        assert_eq!(
            launch.files,
            vec![
                PathBuf::from("作品/my picture.psd"),
                PathBuf::from("mcp"),
                PathBuf::from("other.psb")
            ]
        );
        assert!(!launch.headless);
        assert!(launch
            .absolute_files()
            .unwrap()
            .iter()
            .all(|p| p.is_absolute()));
    }

    #[test]
    fn explicit_file_arguments_are_never_interpreted_as_options() {
        let (state, rest) = arguments(
            args(&[
                "--state-dir",
                "my state",
                "--open",
                "--state-dir",
                "--",
                "--headless",
                "--open",
            ]),
            PathBuf::from("default"),
        )
        .unwrap();
        assert_eq!(state, PathBuf::from("my state"));
        let launch = Launch::parse(rest).unwrap();
        assert_eq!(
            launch.files,
            vec![
                PathBuf::from("--state-dir"),
                PathBuf::from("--headless"),
                PathBuf::from("--open")
            ]
        );
        assert!(!launch.headless);
    }

    #[test]
    fn invalid_launches_fail_instead_of_silently_ignoring_files() {
        for values in [
            &["--open"][..],
            &["--unknown"],
            &["--open", ""],
            &["--headless", "image.png"],
        ] {
            assert!(Launch::parse(args(values)).is_err());
        }
        assert!(arguments(args(&["--state-dir"]), PathBuf::new()).is_err());
        assert!(Launch::parse(vec![OsString::from("image.png"); MAX_PENDING_FILES + 1]).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn local_launch_parser_keeps_non_unicode_os_paths() {
        use std::os::unix::ffi::OsStringExt;
        let path = OsString::from_vec(b"image-\xff.psd".to_vec());
        let launch = Launch::parse(vec![OsString::from("--open"), path.clone()]).unwrap();
        assert_eq!(launch.files, vec![PathBuf::from(path)]);
    }

    #[test]
    fn activation_only_queues_and_never_replaces_dirty_documents() {
        let root = Arc::new(Mutex::new(Engine::new()));
        root.lock().unwrap().doc.revision = 7;
        let before = root.lock().unwrap().state();
        let path = std::env::temp_dir().join("作品 with spaces.psd");
        assert!(
            enqueue(&root, vec![path.clone()]).is_err(),
            "headless instances reject native activation"
        );
        enable(&root);
        let result = server::dispatch(&root, "native_open", &json!({"paths":[path]})).unwrap();
        assert_eq!(result["queued"], 1);
        assert_eq!(root.lock().unwrap().state(), before);
        assert_eq!(workspace::handles(&root).len(), 1);
        assert_eq!(next(&workspace::attach(&root)), Some(path));
        assert!(next(&workspace::attach(&root)).is_none());
    }

    #[test]
    fn invalid_or_overfull_requests_are_rejected_as_a_whole() {
        let root = Arc::new(Mutex::new(Engine::new()));
        enable(&root);
        let path = std::env::temp_dir().join("image.psd");
        for request in [
            json!({}),
            json!({"paths":[]}),
            json!({"paths":[path, 5]}),
            json!({"paths":["relative.psd"]}),
        ] {
            assert!(server::dispatch(&root, "native_open", &request).is_err());
            assert!(next(&workspace::attach(&root)).is_none());
        }
        enqueue(&root, vec![path.clone(); MAX_PENDING_FILES]).unwrap();
        assert!(enqueue(&root, vec![path.clone()]).is_err());
        let registry = workspace::attach(&root);
        assert_eq!(
            registry.lock().unwrap().activation.files.len(),
            MAX_PENDING_FILES
        );
        registry.lock().unwrap().exiting = true;
        assert!(next(&registry).is_none());
        assert!(enqueue(&root, vec![path]).is_err());
    }

    #[test]
    fn second_instance_uses_the_authenticated_listener_and_preserves_path_text() {
        let root = Arc::new(Mutex::new(Engine::new()));
        enable(&root);
        let directory = std::env::temp_dir().join(format!("pb-activation-{}", crate::engine::id()));
        assert!(!server::activate_existing(&directory, &[]).unwrap());
        let connection = server::start(root.clone(), directory.clone()).unwrap();
        let path = directory.join("作品 with spaces.psb");
        let payload = json!({"method":"native_open","params":{"paths":[path]}});
        let url = format!("http://127.0.0.1:{}/rpc", connection.port);
        let rejected = ureq::post(&url).send_json(payload.clone());
        assert!(matches!(rejected, Err(ureq::Error::Status(401, _))));
        assert!(next(&workspace::attach(&root)).is_none());
        let rejected = ureq::post(&url)
            .set("Authorization", &format!("Bearer {}", connection.token))
            .set("Origin", "https://untrusted.example")
            .send_json(payload);
        assert!(matches!(rejected, Err(ureq::Error::Status(403, _))));
        assert!(next(&workspace::attach(&root)).is_none());
        assert!(server::activate_existing(&directory, &[path.clone()]).unwrap());
        assert_eq!(next(&workspace::attach(&root)), Some(path));
        assert!(next(&workspace::attach(&root)).is_none());
        assert_eq!(workspace::handles(&root).len(), 1);
        drop(connection);
        let _ = std::fs::remove_dir_all(directory);
    }
}
