//! Bounded local-only frames with kernel peer PID binding. A name/nonce alone
//! never authenticates a participant or grants update authority.
use super::process::Process;
use super::*;
use ::windows::{
    core::PCWSTR,
    Win32::{
        Foundation::{HANDLE, INVALID_HANDLE_VALUE},
        Storage::FileSystem::{FILE_FLAG_FIRST_PIPE_INSTANCE, PIPE_ACCESS_DUPLEX},
        System::Pipes::*,
    },
};
use std::os::windows::{
    ffi::OsStrExt,
    fs::OpenOptionsExt,
    io::{AsRawHandle, FromRawHandle},
};

pub(super) struct Pipe {
    pub(super) name: String,
    file: File,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Value};
    use std::{
        os::windows::process::CommandExt,
        process::{Child, Command, Stdio},
    };
    struct Owned(Child);
    impl Drop for Owned {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    #[test]
    fn actual_named_pipe_binds_the_owned_peer_and_refuses_oversized_or_foreign_frames() {
        for foreign in [false, true] {
            let mut pipe = Pipe::server().unwrap();
            let mut child = Owned(
                Command::new(std::env::current_exe().unwrap())
                    .args([
                        "--exact",
                        "updates::windows::pipe::tests::pipe_child",
                        "--ignored",
                        "--nocapture",
                    ])
                    .env("PEERBRUSH_NATIVE_PIPE_FIXTURE", &pipe.name)
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .creation_flags(0x08000000)
                    .spawn()
                    .unwrap(),
            );
            let peer = Process::child(&child.0).unwrap();
            let deadline = Instant::now() + Duration::from_secs(15);
            if foreign {
                let wrong = Process::open(std::process::id()).unwrap();
                assert!(pipe.accept(&wrong, deadline).is_err());
                continue;
            }
            pipe.accept(&peer, deadline).unwrap();
            assert!(pipe
                .send(&"x".repeat(SMALL as usize + 1), &peer, deadline)
                .is_err());
            pipe.send(&json!({"protocol":1,"nonce":pipe.name}), &peer, deadline)
                .unwrap();
            let reply: Value = pipe.receive(&peer, deadline).unwrap();
            assert_eq!(reply, json!({"protocol":1,"nonce":pipe.name}));
            pipe.send(&true, &peer, deadline).unwrap();
            loop {
                if let Some(status) = child.0.try_wait().unwrap() {
                    assert!(status.success());
                    break;
                }
                assert!(Instant::now() < deadline);
                std::thread::sleep(Duration::from_millis(10));
            }
            assert!(peer.exited().unwrap());
        }
    }
    #[test]
    fn local_names_and_unconnected_accept_are_bounded() {
        for name in [
            r"\\remote\pipe\PeerBrush.Update.00000000-0000-0000-0000-000000000000",
            "https://example.invalid",
            r"\\.\pipe\PeerBrush.Update.foreign",
        ] {
            assert!(Pipe::valid(name).is_err());
        }
        let pipe = Pipe::server().unwrap();
        assert!(pipe
            .accept(
                &Process::open(std::process::id()).unwrap(),
                Instant::now() + Duration::from_millis(20)
            )
            .is_err());
    }
    #[test]
    #[ignore = "Owned native pipe-contract child; never deployment or profile authority"]
    fn pipe_child() {
        let Some(name) = std::env::var_os("PEERBRUSH_NATIVE_PIPE_FIXTURE") else {
            return;
        };
        let deadline = Instant::now() + Duration::from_secs(15);
        let (mut pipe, peer) = Pipe::connect(name.to_str().unwrap(), deadline).unwrap();
        assert_eq!(
            fs::canonicalize(peer.image().unwrap()).unwrap(),
            fs::canonicalize(std::env::current_exe().unwrap()).unwrap()
        );
        let value: Value = pipe.receive(&peer, deadline).unwrap();
        assert_eq!(value, json!({"protocol":1,"nonce":pipe.name}));
        pipe.send(&value, &peer, deadline).unwrap();
        // Keep the peer alive until the receiver has consumed the acknowledged
        // frame; an exit observed before framing is intentionally a refusal.
        let done: bool = pipe.receive(&peer, deadline).unwrap();
        assert!(done);
    }
}
impl Pipe {
    fn valid(name: &str) -> Result<()> {
        let id = name
            .strip_prefix(r"\\.\pipe\PeerBrush.Update.")
            .ok_or(STAGE_HOLD)?;
        if uuid::Uuid::parse_str(id)
            .map_err(|_| STAGE_HOLD)?
            .to_string()
            != id
        {
            return Err(STAGE_HOLD.into());
        }
        Ok(())
    }
    pub(super) fn server() -> Result<Self> {
        let name = format!(r"\\.\pipe\PeerBrush.Update.{}", uuid::Uuid::new_v4());
        let wide: Vec<_> = std::ffi::OsStr::new(&name)
            .encode_wide()
            .chain(Some(0))
            .collect();
        let handle = unsafe {
            CreateNamedPipeW(
                PCWSTR(wide.as_ptr()),
                PIPE_ACCESS_DUPLEX | FILE_FLAG_FIRST_PIPE_INSTANCE,
                PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_NOWAIT | PIPE_REJECT_REMOTE_CLIENTS,
                1,
                (SMALL * 4) as u32,
                (SMALL * 4) as u32,
                0,
                None,
            )
        };
        if handle == INVALID_HANDLE_VALUE {
            return Err(STAGE_HOLD.into());
        }
        Ok(Self {
            name,
            file: unsafe { File::from_raw_handle(handle.0) },
        })
    }
    pub(super) fn connect(name: &str, deadline: Instant) -> Result<(Self, Process)> {
        Self::valid(name)?;
        let file = loop {
            match OpenOptions::new().read(true).write(true).open(name) {
                Ok(file) => break file,
                Err(error)
                    if [2, 231].contains(&error.raw_os_error().unwrap_or(0))
                        && Instant::now() < deadline =>
                {
                    std::thread::sleep(Duration::from_millis(10))
                }
                Err(_) => return Err(STAGE_HOLD.into()),
            }
        };
        let pipe = Self {
            name: name.into(),
            file,
        };
        unsafe { SetNamedPipeHandleState(pipe.handle(), Some(&PIPE_NOWAIT), None, None) }
            .map_err(|_| STAGE_HOLD)?;
        let mut pid = 0;
        unsafe { GetNamedPipeServerProcessId(pipe.handle(), &mut pid) }.map_err(|_| STAGE_HOLD)?;
        let server = Process::open(pid)?;
        if server.exited()? {
            return Err(STAGE_HOLD.into());
        }
        Ok((pipe, server))
    }
    fn handle(&self) -> HANDLE {
        HANDLE(self.file.as_raw_handle())
    }
    pub(super) fn accept(&self, peer: &Process, deadline: Instant) -> Result<()> {
        loop {
            if peer.exited()? || Instant::now() >= deadline {
                return Err(STAGE_HOLD.into());
            }
            let connected = match unsafe { ConnectNamedPipe(self.handle(), None) } {
                Ok(()) => true,
                Err(error) if error.code().0 as u32 & 0xffff == 535 => true,
                Err(error) if error.code().0 as u32 & 0xffff == 536 => false,
                Err(_) => return Err(STAGE_HOLD.into()),
            };
            if connected {
                let mut pid = 0;
                unsafe { GetNamedPipeClientProcessId(self.handle(), &mut pid) }
                    .map_err(|_| STAGE_HOLD)?;
                if pid != peer.pid || peer.exited()? {
                    return Err(STAGE_HOLD.into());
                }
                return Ok(());
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    fn live(peer: &Process, deadline: Instant) -> Result<()> {
        if peer.exited()? || Instant::now() >= deadline {
            Err(STAGE_HOLD.into())
        } else {
            Ok(())
        }
    }
    pub(super) fn send(
        &mut self,
        value: &impl Serialize,
        peer: &Process,
        deadline: Instant,
    ) -> Result<()> {
        let bytes = serde_json::to_vec(value).map_err(|_| STAGE_HOLD)?;
        if bytes.is_empty() || bytes.len() as u64 > SMALL {
            return Err(STAGE_HOLD.into());
        }
        let mut frame = (bytes.len() as u32).to_le_bytes().to_vec();
        frame.extend(bytes);
        let mut offset = 0;
        while offset < frame.len() {
            Self::live(peer, deadline)?;
            match self.file.write(&frame[offset..]) {
                Ok(count) => offset += count,
                Err(e) if e.raw_os_error() == Some(232) => {}
                Err(_) => return Err(STAGE_HOLD.into()),
            }
            if offset < frame.len() {
                std::thread::sleep(Duration::from_millis(10));
            }
        }
        Ok(())
    }
    fn exact(&mut self, bytes: &mut [u8], peer: &Process, deadline: Instant) -> Result<()> {
        let mut offset = 0;
        while offset < bytes.len() {
            Self::live(peer, deadline)?;
            let mut available = 0;
            unsafe { PeekNamedPipe(self.handle(), None, 0, None, Some(&mut available), None) }
                .map_err(|_| STAGE_HOLD)?;
            if u64::from(available) > SMALL + 4 {
                return Err(STAGE_HOLD.into());
            }
            let count = (bytes.len() - offset).min(available as usize);
            if count > 0 {
                let received = self
                    .file
                    .read(&mut bytes[offset..offset + count])
                    .map_err(|_| STAGE_HOLD)?;
                if received == 0 {
                    return Err(STAGE_HOLD.into());
                }
                offset += received;
            } else {
                std::thread::sleep(Duration::from_millis(10));
            }
        }
        Ok(())
    }
    pub(super) fn receive<T: serde::de::DeserializeOwned>(
        &mut self,
        peer: &Process,
        deadline: Instant,
    ) -> Result<T> {
        let mut length = [0; 4];
        self.exact(&mut length, peer, deadline)?;
        let length = u32::from_le_bytes(length) as usize;
        if length == 0 || length as u64 > SMALL {
            return Err(STAGE_HOLD.into());
        }
        let mut bytes = vec![0; length];
        self.exact(&mut bytes, peer, deadline)?;
        serde_json::from_slice(&bytes).map_err(|_| STAGE_HOLD.into())
    }
}
