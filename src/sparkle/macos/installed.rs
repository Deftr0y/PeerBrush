//! Worker-only signature inspection and owned normalization, before any load.
use super::*;
use crate::sparkle::check::{ApprovedCheck, OBJECTS};
use std::{
    collections::BTreeMap,
    fs::OpenOptions,
    io::{Read, Seek, SeekFrom, Write},
    os::unix::fs::{DirBuilderExt, OpenOptionsExt},
    path::PathBuf,
    process::Stdio,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};

#[derive(Clone, PartialEq, Eq)]
struct Stamp {
    device: u64,
    inode: u64,
    mode: u32,
    links: u64,
    uid: u32,
    gid: u32,
    bytes: u64,
    modified: (i64, i64),
    changed: (i64, i64),
}
fn stamp(path: &Path) -> Result<Stamp, String> {
    let value =
        fs::symlink_metadata(path).map_err(|_| "App update verification input is unavailable")?;
    Ok(Stamp {
        device: value.dev(),
        inode: value.ino(),
        mode: value.mode(),
        links: value.nlink(),
        uid: value.uid(),
        gid: value.gid(),
        bytes: value.len(),
        modified: (value.mtime(), value.mtime_nsec()),
        changed: (value.ctime(), value.ctime_nsec()),
    })
}

fn admin_group() -> Option<u32> {
    let mut record = std::mem::MaybeUninit::<libc::group>::uninit();
    let mut result = std::ptr::null_mut();
    let mut bytes = vec![0_u8; 64 * 1024];
    // Reentrant bounded lookup; no account or permission changes. Do not guess
    // a numeric admin GID or treat another writable group as administrator.
    let status = unsafe {
        libc::getgrnam_r(
            c"admin".as_ptr(),
            record.as_mut_ptr(),
            bytes.as_mut_ptr().cast(),
            bytes.len(),
            &mut result,
        )
    };
    if status != 0 || result.is_null() {
        return None;
    }
    Some(unsafe { record.assume_init() }.gr_gid)
}

fn safe_ancestor(path: &Path, app: &Path, value: &Stamp, user: u32, admin: Option<u32>) -> bool {
    if value.mode & u32::from(libc::S_IFMT) != u32::from(libc::S_IFDIR)
        || (value.uid != 0 && value.uid != user)
    {
        return false;
    }
    if value.mode & 0o022 == 0 {
        return true;
    }
    // The systemwide installation container is administrator-managed, while
    // the exact app and its descendants remain subject to the stricter policy.
    path == Path::new("/Applications")
        && !path.starts_with(app)
        && value.uid == 0
        && value.mode & 0o7777 == 0o775
        && admin == Some(value.gid)
}

pub(super) struct Verified {
    pub framework: PathBuf,
    snapshots: Vec<(PathBuf, Stamp)>,
}
impl Verified {
    pub fn recheck(&self) -> Result<(), String> {
        for (path, before) in &self.snapshots {
            if stamp(path)? != *before {
                return Err(
                    "The app changed during update verification; restart before checking again"
                        .into(),
                );
            }
        }
        Ok(())
    }
}

struct Scratch {
    path: PathBuf,
    identity: Stamp,
    sequence: u32,
    cancel: Arc<AtomicBool>,
}
impl Scratch {
    fn new(cancel: Arc<AtomicBool>) -> Result<Self, String> {
        let parent = std::env::temp_dir()
            .canonicalize()
            .map_err(|_| "Update inspection storage is unavailable")?;
        let path = parent.join(format!(
            "peerbrush-sparkle-inspect-{}",
            uuid::Uuid::new_v4()
        ));
        fs::DirBuilder::new()
            .mode(0o700)
            .create(&path)
            .map_err(|_| "Could not create private update inspection storage")?;
        let identity = stamp(&path)?;
        if identity.uid != unsafe { libc::geteuid() } || identity.mode & 0o7777 != 0o700 {
            return Err("Update inspection storage is not privately owned".into());
        }
        Ok(Self {
            path,
            identity,
            sequence: 0,
            cancel,
        })
    }
    fn command(&mut self, args: &[&str], path: &Path) -> Result<(Vec<u8>, Vec<u8>), String> {
        let suffix = self.sequence;
        self.sequence += 1;
        let mut stdout = OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(self.path.join(format!("stdout-{suffix}")))
            .map_err(|_| "Update verification output unavailable")?;
        let mut stderr = OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(self.path.join(format!("stderr-{suffix}")))
            .map_err(|_| "Update verification output unavailable")?;
        let mut child = Command::new("/usr/bin/codesign")
            .args(args)
            .arg(path)
            .current_dir(&self.path)
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("LANG", "C")
            .env("TMPDIR", &self.path)
            .stdin(Stdio::null())
            .stdout(
                stdout
                    .try_clone()
                    .map_err(|_| "Update output unavailable")?,
            )
            .stderr(
                stderr
                    .try_clone()
                    .map_err(|_| "Update output unavailable")?,
            )
            .spawn()
            .map_err(|_| "Native app update verification could not start")?;
        let deadline = Instant::now() + Duration::from_secs(30);
        let result = (|| {
            loop {
                if self.cancel.load(Ordering::Acquire)
                    || stdout
                        .metadata()
                        .map_err(|_| "Update output unavailable")?
                        .len()
                        > 16384
                    || stderr
                        .metadata()
                        .map_err(|_| "Update output unavailable")?
                        .len()
                        > 16384
                    || Instant::now() >= deadline
                {
                    return Err("Native app update verification exceeded its budget".to_string());
                }
                match child
                    .try_wait()
                    .map_err(|_| "Native app update verification failed")?
                {
                    Some(status) if status.success() => break,
                    Some(_) => return Err("Native app update signature verification failed".into()),
                    None => std::thread::sleep(Duration::from_millis(20)),
                }
            }
            let read = |file: &mut fs::File| -> Result<Vec<u8>, String> {
                file.seek(SeekFrom::Start(0))
                    .map_err(|_| "Update output unavailable")?;
                let mut bytes = Vec::new();
                file.take(16385)
                    .read_to_end(&mut bytes)
                    .map_err(|_| "Update output unavailable")?;
                if bytes.len() > 16384 {
                    return Err("Update output exceeds its budget".into());
                }
                Ok(bytes)
            };
            Ok((read(&mut stdout)?, read(&mut stderr)?))
        })();
        if result.is_err() {
            let _ = child.kill();
            let _ = child.wait();
        }
        result
    }
    fn normalized(&mut self, source: &Path) -> Result<String, String> {
        let target = self.path.join(format!("code-{}", self.sequence));
        let bytes = bounded(source, 16 * 1024 * 1024)?;
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o700)
            .open(&target)
            .map_err(|_| "Owned native normalization unavailable")?;
        file.write_all(&bytes)
            .map_err(|_| "Owned native normalization unavailable")?;
        drop(file);
        self.command(&["--remove-signature"], &target)?;
        sha256(&bounded(&target, 16 * 1024 * 1024)?)
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        // Recursive cleanup is restricted to this exact new UUID directory.
        // Its identity/owner/mode must still be ours; never follow a replaced root.
        if let Ok(value) = stamp(&self.path) {
            if value.device == self.identity.device
                && value.inode == self.identity.inode
                && value.uid == self.identity.uid
                && value.mode == self.identity.mode
            {
                let _ = fs::remove_dir_all(&self.path);
            }
        }
    }
}

pub(super) fn bounded(path: &Path, limit: u64) -> Result<Vec<u8>, String> {
    let before = stamp(path)?;
    if before.mode & u32::from(libc::S_IFMT) != u32::from(libc::S_IFREG)
        || before.links != 1
        || before.bytes > limit
    {
        return Err("App update verification requires bounded regular files".into());
    }
    let input = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
        .map_err(|_| "App update verification input unavailable")?;
    let metadata = input
        .metadata()
        .map_err(|_| "App update verification input unavailable")?;
    if metadata.dev() != before.device || metadata.ino() != before.inode {
        return Err("App update input changed during inspection".into());
    }
    let mut bytes = Vec::new();
    input
        .take(limit + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "App update input unreadable")?;
    if bytes.len() as u64 != before.bytes || stamp(path)? != before {
        return Err("App update input changed during inspection".into());
    }
    Ok(bytes)
}

fn plist(bytes: &[u8]) -> Result<Value, String> {
    if bytes.len() > 256 * 1024 {
        return Err("App update plist exceeds its budget".into());
    }
    let data = objc2_foundation::NSData::with_bytes(bytes);
    let value: Option<Retained<AnyObject>> = unsafe {
        msg_send![class!(NSPropertyListSerialization), propertyListWithData: &*data, options: 0_usize, format: std::ptr::null_mut::<usize>(), error: std::ptr::null_mut::<*mut NSError>()]
    };
    let value = value.ok_or("Invalid native update property list")?;
    let json: Option<Retained<objc2_foundation::NSData>> = unsafe {
        msg_send![class!(NSJSONSerialization), dataWithJSONObject: &*value, options: 0_usize, error: std::ptr::null_mut::<*mut NSError>()]
    };
    let json = json.ok_or("Unsupported native update property list")?;
    if json.length() > 256 * 1024 {
        return Err("Native update property list exceeds its budget".into());
    }
    let mut bytes = vec![0u8; json.length()];
    unsafe {
        let _: () =
            msg_send![&*json, getBytes: bytes.as_mut_ptr().cast::<c_void>(), length: bytes.len()];
    }
    serde_json::from_slice(&bytes).map_err(|_| "Invalid native update property list".into())
}

fn native_object(
    scratch: &mut Scratch,
    path: &Path,
    identifier: &str,
    entitlements: &BTreeMap<String, Value>,
    team: &str,
) -> Result<(), String> {
    let requirement=format!("=anchor apple generic and certificate leaf[subject.OU] = \"{team}\" and certificate leaf[field.1.2.840.113635.100.6.1.13] exists");
    scratch.command(
        &["--verify", "--strict", "--test-requirement", &requirement],
        path,
    )?;
    let (stdout, stderr) = scratch.command(&["--display", "--verbose=4"], path)?;
    let text = String::from_utf8([stdout, stderr].concat())
        .map_err(|_| "Invalid native app signature metadata")?;
    let lines = text.lines().collect::<Vec<_>>();
    let one = |prefix: &str| {
        lines
            .iter()
            .filter_map(|line| line.strip_prefix(prefix))
            .collect::<Vec<_>>()
    };
    if one("Identifier=") != [identifier]
        || one("TeamIdentifier=") != [team]
        || one("Authority=").first().is_none_or(|authority| {
            !authority.starts_with("Developer ID Application: ")
                || !authority.ends_with(&format!(" ({team})"))
        })
        || one("Timestamp=").len() != 1
        || one("Timestamp=")[0].is_empty()
        || one("CodeDirectory ").len() != 1
        || !one("CodeDirectory ")[0]
            .split_once('(')
            .is_some_and(|(_, flags)| {
                flags
                    .split(')')
                    .next()
                    .is_some_and(|flags| flags.split(',').any(|flag| flag == "runtime"))
            })
    {
        return Err("Native updater publisher, timestamp, runtime or identifier differs".into());
    }
    let (stdout, _) = scratch.command(&["--display", "--entitlements", "-", "--xml"], path)?;
    let actual = if stdout.is_empty() {
        Value::Object(Default::default())
    } else {
        plist(&stdout)?
    };
    if actual != serde_json::to_value(entitlements).map_err(|_| "Invalid reviewed entitlements")? {
        return Err("Native updater entitlements differ from reviewed policy".into());
    }
    Ok(())
}

pub(super) fn verify(
    app: PathBuf,
    configuration: ApprovedCheck,
    cancel: Arc<AtomicBool>,
) -> Result<Verified, String> {
    objc2::rc::autoreleasepool(|_| {
        if cancel.load(Ordering::Acquire) {
            return Err("App update verification cancelled".into());
        }
        let framework = app.join("Contents/Frameworks/Sparkle.framework");
        let notice = app.join("Contents/Resources/licenses/sparkle-2.10.0/LICENSE");
        let mut snapshots = Vec::new();
        fn snapshot_tree(path: &Path, out: &mut Vec<(PathBuf, Stamp)>) -> Result<(), String> {
            let value = stamp(path)?;
            if out.len() >= 200
                || (value.mode & u32::from(libc::S_IFMT) != u32::from(libc::S_IFLNK)
                    && value.mode & 0o022 != 0)
                || (value.uid != 0 && value.uid != unsafe { libc::geteuid() })
            {
                return Err(
                    "App update inputs must be bounded and not writable by another user".into(),
                );
            }
            let directory = value.mode & u32::from(libc::S_IFMT) == u32::from(libc::S_IFDIR);
            out.push((path.into(), value));
            if directory {
                for child in fs::read_dir(path).map_err(|_| "App update inputs unavailable")? {
                    snapshot_tree(
                        &child.map_err(|_| "App update input unavailable")?.path(),
                        out,
                    )?;
                }
            }
            Ok(())
        }
        let admin = admin_group();
        for ancestor in app.ancestors() {
            let value = stamp(ancestor)?;
            if !safe_ancestor(ancestor, &app, &value, unsafe { libc::geteuid() }, admin) {
                return Err(
                    "App update bundle ancestors must be unchanged safe directories".into(),
                );
            }
            snapshots.push((ancestor.into(), value));
        }
        snapshot_tree(&framework, &mut snapshots)?;
        for path in [
            app.join("Contents/Info.plist"),
            app.join("Contents/MacOS/peerbrush"),
            notice.clone(),
        ] {
            let value = stamp(&path)?;
            if value.mode & u32::from(libc::S_IFMT) != u32::from(libc::S_IFREG)
                || value.links != 1
                || value.mode & 0o022 != 0
                || (value.uid != 0 && value.uid != unsafe { libc::geteuid() })
            {
                return Err(
                    "App update configuration/code must be safely owned regular files".into(),
                );
            }
            for ancestor in path
                .parent()
                .ok_or("App input parent unavailable")?
                .ancestors()
                .take_while(|ancestor| ancestor.starts_with(&app))
            {
                let value = stamp(ancestor)?;
                if value.mode & u32::from(libc::S_IFMT) != u32::from(libc::S_IFDIR)
                    || value.mode & 0o022 != 0
                    || (value.uid != 0 && value.uid != unsafe { libc::geteuid() })
                {
                    return Err(
                        "App update input ancestors must be safely owned directories".into(),
                    );
                }
                snapshots.push((ancestor.into(), value));
            }
            snapshots.push((path.clone(), value));
        }
        let verified = Verified {
            framework: framework.clone(),
            snapshots,
        };
        super::verify_inventory(&framework, &notice, true)?;
        let info = plist(&bounded(&app.join("Contents/Info.plist"), 256 * 1024)?)?;
        for (key, value) in [
            ("CFBundleIdentifier", Value::from("com.peerbrush.PeerBrush")),
            (
                "CFBundleShortVersionString",
                Value::from(env!("CARGO_PKG_VERSION")),
            ),
            ("SUFeedURL", Value::from(configuration.feed_url.as_str())),
            (
                "SUPublicEDKey",
                Value::from(configuration.public_ed_key.as_str()),
            ),
            ("SUEnableAutomaticChecks", Value::Bool(false)),
            ("SUAutomaticallyUpdate", Value::Bool(false)),
            ("SUAllowsAutomaticUpdates", Value::Bool(false)),
            ("SUEnableSystemProfiling", Value::Bool(false)),
            ("SUVerifyUpdateBeforeExtraction", Value::Bool(true)),
            ("SURequireSignedFeed", Value::Bool(true)),
            ("SUSignedFeedFailureExpirationInterval", Value::from(0)),
        ] {
            if info[key] != value {
                return Err(
                    "Installed app update configuration differs from reviewed source".into(),
                );
            }
        }
        let mut scratch = Scratch::new(cancel.clone())?;
        // A signed file at a replaced path is insufficient to identify the
        // running app. codesign's numeric subject performs dynamic validation.
        let requirement=format!("=identifier \"com.peerbrush.PeerBrush\" and anchor apple generic and certificate leaf[subject.OU] = \"{}\" and certificate leaf[field.1.2.840.113635.100.6.1.13] exists",configuration.publisher_team_id);
        scratch.command(
            &["--verify", "--strict", "--test-requirement", &requirement],
            Path::new(&std::process::id().to_string()),
        )?;
        native_object(
            &mut scratch,
            &app,
            "com.peerbrush.PeerBrush",
            &BTreeMap::new(),
            &configuration.publisher_team_id,
        )?;
        for (name, code) in OBJECTS {
            let policy = &configuration.objects[name];
            native_object(
                &mut scratch,
                &framework.join(name),
                &policy.identifier,
                &policy.entitlements,
                &configuration.publisher_team_id,
            )?;
            if scratch.normalized(&framework.join(code))? != policy.unsigned_sha256 {
                return Err(
                    "Native updater code differs from independently reviewed unsigned bytes".into(),
                );
            }
        }
        if cancel.load(Ordering::Acquire) {
            return Err("App update verification cancelled".into());
        }
        verified.recheck()?;
        Ok(verified)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn only_exact_root_admin_system_application_container_can_be_group_writable() {
        let app = Path::new("/Applications/PeerBrush.app");
        let mut value = Stamp {
            device: 1,
            inode: 2,
            mode: u32::from(libc::S_IFDIR) | 0o775,
            links: 1,
            uid: 0,
            gid: 80,
            bytes: 0,
            modified: (0, 0),
            changed: (0, 0),
        };
        assert!(safe_ancestor(
            Path::new("/Applications"),
            app,
            &value,
            501,
            Some(80)
        ));
        for path in [
            app,
            Path::new("/Applications/Other"),
            Path::new("/Users/Shared"),
        ] {
            assert!(!safe_ancestor(path, app, &value, 501, Some(80)));
        }
        assert!(!safe_ancestor(
            Path::new("/Applications"),
            app,
            &value,
            501,
            None
        ));
        assert!(!safe_ancestor(
            Path::new("/Applications"),
            app,
            &value,
            501,
            Some(81)
        ));
        value.uid = 501;
        assert!(!safe_ancestor(
            Path::new("/Applications"),
            app,
            &value,
            501,
            Some(80)
        ));
        value.uid = 0;
        value.mode = u32::from(libc::S_IFDIR) | 0o777;
        assert!(!safe_ancestor(
            Path::new("/Applications"),
            app,
            &value,
            501,
            Some(80)
        ));
        value.mode = u32::from(libc::S_IFDIR) | 0o755;
        assert!(safe_ancestor(app, app, &value, 501, None));
    }
}
