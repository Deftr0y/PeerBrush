//! Windows share-mode gate, independent of the parent's byte-range profile lock.
use super::*;
use std::os::windows::fs::OpenOptionsExt;

const NAME: &str = "settings-update-gate.lock";
const HOLD: &str = "Settings are in use by an authenticated Windows update owner; preserve this profile and retry after recovery.";

pub(crate) struct Gate {
    directory: Directory,
    file: File,
    path: PathBuf,
    identity: (u64, u64),
    owner: bool,
}
impl Gate {
    fn open(config: &Path, admission: bool) -> Result<Self> {
        let directory = Directory::open(config)?;
        let path = config.join(NAME);
        if fs::symlink_metadata(&path).is_ok() {
            regular(&path)?;
        }
        let file = OpenOptions::new()
            .read(true)
            .write(admission)
            .create(admission)
            .truncate(false)
            // Admission requests write access. The owner's read-only handle
            // denies it, including after the original parent's handles close.
            .share_mode(if admission { 3 } else { 1 })
            .custom_flags(0x00200000) // FILE_FLAG_OPEN_REPARSE_POINT
            .open(&path)
            .map_err(|_| HOLD)?;
        let identity = crate::file_safety::identity(&file)?;
        let gate = Self {
            directory,
            file,
            path,
            identity,
            owner: !admission,
        };
        gate.check()?;
        Ok(gate)
    }
    pub(crate) fn admit(config: &Path) -> Result<Self> {
        Self::open(config, true)
    }
    pub(super) fn own(config: &Path) -> Result<Self> {
        Self::open(config, false)
    }
    pub(crate) fn validate_owner(&self, config: &Path) -> Result<()> {
        if !self.owner || self.directory.path != config {
            return Err(HOLD.into());
        }
        self.check()
    }
    pub(super) fn check(&self) -> Result<()> {
        self.directory.check()?;
        regular(&self.path)?;
        let named = crate::file_safety::open_for_identity(&self.path).map_err(|_| HOLD)?;
        if crate::file_safety::identity(&named)? != self.identity
            || crate::file_safety::identity(&self.file)? != self.identity
            || self.file.metadata().map_err(|_| HOLD)?.len() != 0
        {
            return Err(HOLD.into());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        io::BufRead,
        os::windows::process::CommandExt,
        process::{Command, Stdio},
    };
    struct Child(std::process::Child);
    impl Drop for Child {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    #[test]
    fn actual_windows_share_gate_survives_parent_copy_and_blocks_write_delete_admission() {
        let root = std::env::temp_dir().join(format!(
            "peerbrush-native-update-gate-{}",
            crate::engine::id()
        ));
        fs::create_dir(&root).unwrap();
        drop(Gate::admit(&root).unwrap());
        let parent = Gate::own(&root).unwrap();
        let mut child = Child(
            Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "updates::windows::gate::tests::gate_child",
                    "--ignored",
                    "--nocapture",
                ])
                .env("PEERBRUSH_NATIVE_GATE_FIXTURE", &root)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .creation_flags(0x08000000)
                .spawn()
                .unwrap(),
        );
        let mut stdout = std::io::BufReader::new(child.0.stdout.take().unwrap());
        let kernel = super::super::process::Process::child(&child.0).unwrap();
        assert_eq!(kernel.pid, child.0.id());
        assert!(kernel.created > 0 && !kernel.exited().unwrap());
        assert_eq!(
            fs::canonicalize(kernel.image().unwrap()).unwrap(),
            fs::canonicalize(std::env::current_exe().unwrap()).unwrap()
        );
        assert_eq!(
            super::super::process::Process::open(child.0.id())
                .unwrap()
                .created,
            kernel.created
        );
        let (send, receive) = mpsc::channel();
        std::thread::spawn(move || {
            // Keep draining through child exit. Closing stdout at Ready makes
            // libtest's subsequent result write fail with a broken pipe.
            for _ in 0..32 {
                let mut line = String::new();
                if stdout.read_line(&mut line).unwrap_or(0) == 0 {
                    break;
                }
                if line.trim_end().ends_with("PeerBrushNativeGateReadyV1") {
                    let _ = send.send(());
                }
            }
        });
        let ready = receive.recv_timeout(Duration::from_secs(10)).is_ok();
        assert!(
            ready,
            "actual child must independently retain the native gate"
        );
        drop(parent);
        assert!(Gate::admit(&root).is_err());
        assert!(OpenOptions::new()
            .write(true)
            .open(root.join(NAME))
            .is_err());
        assert!(fs::rename(root.join(NAME), root.join("foreign.lock")).is_err());
        assert!(fs::remove_file(root.join(NAME)).is_err());
        child.0.stdin.take().unwrap().write_all(b"release").unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(status) = child.0.try_wait().unwrap() {
                assert!(status.success());
                break;
            }
            assert!(Instant::now() < deadline, "owned child did not finish");
            std::thread::sleep(Duration::from_millis(10));
        }
        drop(Gate::admit(&root).unwrap());
        assert!(
            kernel.exited().unwrap(),
            "the held kernel object, not a journal PID, proves actual exit"
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    #[ignore = "Owned child of the native gate contract, not an installation test"]
    fn gate_child() {
        let Some(root) = std::env::var_os("PEERBRUSH_NATIVE_GATE_FIXTURE") else {
            return;
        };
        let gate = Gate::own(Path::new(&root)).unwrap();
        println!("PeerBrushNativeGateReadyV1");
        std::io::stdout().flush().unwrap();
        let mut signal = [0; 7];
        std::io::stdin().read_exact(&mut signal).unwrap();
        assert_eq!(&signal, b"release");
        gate.check().unwrap();
    }
}
