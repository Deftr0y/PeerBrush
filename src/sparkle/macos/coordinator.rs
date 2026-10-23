//! Same-signed-binary capture owner. It cannot start Sparkle or authorize exit.
use super::{checkpoint, process_identity};
use crate::{settings::UpdateProfile, sparkle::check::ApprovedCheck};
use serde::{Deserialize, Serialize};
use std::{
    fs::File,
    io::{Read, Write},
    os::{
        fd::{AsRawFd, FromRawFd, OwnedFd},
        unix::net::UnixStream,
    },
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Condvar, Mutex,
    },
    time::{Duration, Instant},
};

const ENTRY: &str = "--peerbrush-update-capture-owner";
const FAILED: &str = "App update capture owner is unavailable; all projects remain open";
const FRAME_LIMIT: usize = 4096;
const DEADLINE: Duration = Duration::from_secs(600);
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    schema: u32,
    nonce: String,
    parent_pid: u32,
    checkpoint: String,
    manifest_sha256: String,
    build: String,
}
#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum ReplyKind {
    Authenticated,
    Captured,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Captured {
    kind: ReplyKind,
    schema: u32,
    nonce: String,
    owner_pid: u32,
    checkpoint: String,
    manifest_sha256: String,
    build: String,
}
fn canonical_hex(value: &str, count: usize) -> bool {
    value.len() == count
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
impl Request {
    fn validate(&self, parent: u32) -> Result<(), String> {
        if self.schema != 1
            || self.parent_pid != parent
            || parent <= 1
            || !canonical_hex(&self.nonce, 64)
            || !canonical_hex(&self.manifest_sha256, 64)
            || uuid::Uuid::parse_str(&self.checkpoint)
                .map_err(|_| FAILED)?
                .to_string()
                != self.checkpoint
            || self.build != crate::build_info::json()
            || self.build.len() > 512
        {
            return Err(FAILED.into());
        }
        Ok(())
    }
}
impl Captured {
    fn validate(&self, request: &Request, child: u32, kind: ReplyKind) -> Result<(), String> {
        if self.kind != kind
            || self.schema != 1
            || self.owner_pid != child
            || child <= 1
            || self.nonce != request.nonce
            || self.checkpoint != request.checkpoint
            || self.manifest_sha256 != request.manifest_sha256
            || self.build != request.build
        {
            return Err(FAILED.into());
        }
        Ok(())
    }
}
fn remaining(deadline: Instant, cancel: &AtomicBool) -> Result<Duration, String> {
    if cancel.load(Ordering::Acquire) {
        return Err(FAILED.into());
    }
    deadline
        .checked_duration_since(Instant::now())
        .filter(|d| !d.is_zero())
        .map(|d| d.min(Duration::from_millis(100)))
        .ok_or_else(|| FAILED.into())
}
fn retry(error: &std::io::Error) -> bool {
    matches!(
        error.kind(),
        std::io::ErrorKind::WouldBlock
            | std::io::ErrorKind::TimedOut
            | std::io::ErrorKind::Interrupted
    )
}
fn exact(
    socket: &mut UnixStream,
    mut bytes: &mut [u8],
    deadline: Instant,
    cancel: &AtomicBool,
) -> Result<(), String> {
    while !bytes.is_empty() {
        socket
            .set_read_timeout(Some(remaining(deadline, cancel)?))
            .map_err(|_| FAILED)?;
        match socket.read(bytes) {
            Ok(0) => return Err(FAILED.into()),
            Ok(count) => bytes = &mut bytes[count..],
            Err(e) if retry(&e) => {}
            Err(_) => return Err(FAILED.into()),
        }
    }
    Ok(())
}
fn receive<T: serde::de::DeserializeOwned>(
    socket: &mut UnixStream,
    deadline: Instant,
    cancel: &AtomicBool,
) -> Result<T, String> {
    let mut length = [0_u8; 4];
    exact(socket, &mut length, deadline, cancel)?;
    let count = u32::from_be_bytes(length) as usize;
    if !(1..=FRAME_LIMIT).contains(&count) {
        return Err(FAILED.into());
    }
    let mut bytes = vec![0; count];
    exact(socket, &mut bytes, deadline, cancel)?;
    serde_json::from_slice(&bytes).map_err(|_| FAILED.into())
}
fn send(
    socket: &mut UnixStream,
    value: &impl Serialize,
    deadline: Instant,
    cancel: &AtomicBool,
) -> Result<(), String> {
    let bytes = serde_json::to_vec(value).map_err(|_| FAILED)?;
    if !(1..=FRAME_LIMIT).contains(&bytes.len()) {
        return Err(FAILED.into());
    }
    let mut output = (bytes.len() as u32).to_be_bytes().to_vec();
    output.extend(bytes);
    let mut bytes = output.as_slice();
    while !bytes.is_empty() {
        socket
            .set_write_timeout(Some(remaining(deadline, cancel)?))
            .map_err(|_| FAILED)?;
        match socket.write(bytes) {
            Ok(0) => return Err(FAILED.into()),
            Ok(count) => bytes = &bytes[count..],
            Err(e) if retry(&e) => {}
            Err(_) => return Err(FAILED.into()),
        }
    }
    Ok(())
}
fn no_sigpipe(socket: &UnixStream) -> Result<(), String> {
    let one = 1_i32;
    if unsafe {
        libc::setsockopt(
            socket.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_NOSIGPIPE,
            (&one as *const i32).cast(),
            std::mem::size_of_val(&one) as libc::socklen_t,
        )
    } != 0
    {
        return Err(FAILED.into());
    }
    Ok(())
}
fn send_lock(socket: &UnixStream, file: &File) -> Result<(), String> {
    let mut marker = [0x50_u8];
    let mut iov = libc::iovec {
        iov_base: marker.as_mut_ptr().cast(),
        iov_len: 1,
    };
    let mut control = [0_usize; 8];
    let mut message: libc::msghdr = unsafe { std::mem::zeroed() };
    message.msg_iov = &mut iov;
    message.msg_iovlen = 1;
    message.msg_control = control.as_mut_ptr().cast();
    message.msg_controllen = unsafe { libc::CMSG_SPACE(std::mem::size_of::<i32>() as u32) };
    unsafe {
        let header = libc::CMSG_FIRSTHDR(&message);
        if header.is_null() {
            return Err(FAILED.into());
        }
        (*header).cmsg_level = libc::SOL_SOCKET;
        (*header).cmsg_type = libc::SCM_RIGHTS;
        (*header).cmsg_len = libc::CMSG_LEN(std::mem::size_of::<i32>() as u32);
        libc::CMSG_DATA(header)
            .cast::<i32>()
            .write_unaligned(file.as_raw_fd());
        if libc::sendmsg(socket.as_raw_fd(), &message, 0) != 1 {
            return Err(FAILED.into());
        }
    }
    Ok(())
}
fn receive_lock(
    socket: &UnixStream,
    deadline: Instant,
    cancel: &AtomicBool,
) -> Result<File, String> {
    loop {
        socket
            .set_read_timeout(Some(remaining(deadline, cancel)?))
            .map_err(|_| FAILED)?;
        let mut marker = [0_u8];
        let mut iov = libc::iovec {
            iov_base: marker.as_mut_ptr().cast(),
            iov_len: 1,
        };
        let mut control = [0_usize; 8];
        let mut message: libc::msghdr = unsafe { std::mem::zeroed() };
        message.msg_iov = &mut iov;
        message.msg_iovlen = 1;
        message.msg_control = control.as_mut_ptr().cast();
        message.msg_controllen = std::mem::size_of_val(&control) as _;
        let count = unsafe { libc::recvmsg(socket.as_raw_fd(), &mut message, 0) };
        if count < 0 {
            if retry(&std::io::Error::last_os_error()) {
                continue;
            }
            return Err(FAILED.into());
        }
        let mut files = Vec::new();
        let mut valid = true;
        unsafe {
            let mut header = libc::CMSG_FIRSTHDR(&message);
            while !header.is_null() {
                let base = libc::CMSG_LEN(0) as usize;
                let end = control.as_ptr() as usize
                    + (message.msg_controllen as usize).min(std::mem::size_of_val(&control));
                let available = end.saturating_sub(header as usize);
                if available < std::mem::size_of::<libc::cmsghdr>() {
                    valid = false;
                    break;
                }
                let length = (*header).cmsg_len as usize;
                if length < base {
                    valid = false;
                    break;
                }
                let received = length.min(available).saturating_sub(base);
                if (*header).cmsg_level != libc::SOL_SOCKET
                    || (*header).cmsg_type != libc::SCM_RIGHTS
                {
                    valid = false;
                    if length > available {
                        break;
                    }
                    header = libc::CMSG_NXTHDR(&message, header);
                    continue;
                }
                if length > available || received % std::mem::size_of::<i32>() != 0 {
                    valid = false;
                }
                // Even a truncated refused control message must close all file
                // descriptors actually delivered by the kernel into this buffer.
                for index in 0..(received / std::mem::size_of::<i32>()) {
                    let fd = libc::CMSG_DATA(header)
                        .cast::<i32>()
                        .add(index)
                        .read_unaligned();
                    if fd < 0 {
                        valid = false;
                        continue;
                    }
                    let file = File::from_raw_fd(fd);
                    if libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) != 0 {
                        valid = false;
                    }
                    files.push(file);
                }
                if length > available {
                    break;
                }
                header = libc::CMSG_NXTHDR(&message, header);
            }
        }
        if count != 1
            || marker != [0x50]
            || message.msg_flags & libc::MSG_CTRUNC != 0
            || !valid
            || files.len() != 1
        {
            return Err(FAILED.into());
        }
        return Ok(files.pop().unwrap());
    }
}

struct ReapState {
    child: Option<Child>,
    closed: bool,
}
struct Reaper {
    shared: Arc<(Mutex<ReapState>, Condvar)>,
}
impl Reaper {
    fn new() -> Result<Self, String> {
        Self::with_wait(|mut child| {
            let _ = child.wait();
        })
    }
    fn with_wait(wait: impl FnOnce(Child) + Send + 'static) -> Result<Self, String> {
        let shared = Arc::new((
            Mutex::new(ReapState {
                child: None,
                closed: false,
            }),
            Condvar::new(),
        ));
        let worker = shared.clone();
        // This fallible operation must finish before any child can be created.
        std::thread::Builder::new()
            .name("peerbrush-capture-owner-reap".into())
            .spawn(move || {
                let child = {
                    let (state, wake) = &*worker;
                    let mut state = state.lock().unwrap_or_else(|e| e.into_inner());
                    while !state.closed {
                        state = wake.wait(state).unwrap_or_else(|e| e.into_inner());
                    }
                    state.child.take()
                };
                if let Some(child) = child {
                    wait(child);
                }
            })
            .map_err(|_| FAILED)?;
        Ok(Self { shared })
    }
    fn handoff(self, child: Child) {
        // The worker already owns this slot; no thread/channel allocation or
        // fallible send can lose Child during an error or UI-thread drop.
        self.shared
            .0
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .child = Some(child);
        // Drop closes/notifies after releasing the mutex, exactly once.
    }
}
impl Drop for Reaper {
    fn drop(&mut self) {
        self.shared
            .0
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .closed = true;
        self.shared.1.notify_one();
    }
}

/// Dropping the channel asks only our capture process to return. It neither
/// kills a process nor treats its disappearance as native installer absence.
pub(super) struct Owner {
    socket: UnixStream,
    child: Option<(Child, Reaper)>,
}
impl Drop for Owner {
    fn drop(&mut self) {
        let _ = self.socket.shutdown(std::net::Shutdown::Both);
        if let Some((child, reaper)) = self.child.take() {
            reaper.handoff(child);
        }
    }
}
fn spawn_owned(
    socket: UnixStream,
    establish: impl FnOnce() -> Result<Reaper, String>,
    spawn: impl FnOnce() -> Result<Child, String>,
) -> Result<Owner, String> {
    let reaper = establish()?;
    let child = spawn()?;
    Ok(Owner {
        socket,
        child: Some((child, reaper)),
    })
}

pub(super) fn launch(
    copy: &Path,
    profile: &UpdateProfile,
    checkpoint: &str,
    manifest_sha256: String,
    configuration: &ApprovedCheck,
    cancel: &AtomicBool,
) -> Result<Owner, String> {
    if cancel.load(Ordering::Acquire) {
        return Err(FAILED.into());
    }
    let request = Request {
        schema: 1,
        nonce: format!(
            "{}{}",
            uuid::Uuid::new_v4().simple(),
            uuid::Uuid::new_v4().simple()
        ),
        parent_pid: std::process::id(),
        checkpoint: checkpoint.into(),
        manifest_sha256,
        build: crate::build_info::json().into(),
    };
    request.validate(std::process::id())?;
    let deadline = Instant::now() + DEADLINE;
    let inherited = profile.inherited_lock()?;
    let (socket, child_socket) = UnixStream::pair().map_err(|_| FAILED)?;
    no_sigpipe(&socket)?;
    no_sigpipe(&child_socket)?;
    let child_fd: OwnedFd = child_socket.into();
    let mut command = Command::new(copy.join("Contents/MacOS/peerbrush"));
    command
        .arg(ENTRY)
        .current_dir(copy)
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("LANG", "C");
    let home = std::env::var_os("HOME").ok_or(FAILED)?;
    command.env("HOME", home);
    if let Some(tmp) = std::env::var_os("TMPDIR") {
        command.env("TMPDIR", tmp);
    }
    command
        .stdin(Stdio::from(child_fd))
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let mut owner = spawn_owned(socket, Reaper::new, || {
        command.spawn().map_err(|_| FAILED.into())
    })?;
    owner
        .socket
        .set_write_timeout(Some(Duration::from_secs(5)))
        .map_err(|_| FAILED)?;
    send(&mut owner.socket, &request, deadline, cancel)?;
    let response: Captured = receive(&mut owner.socket, deadline, cancel)?;
    let child = &mut owner.child.as_mut().unwrap().0;
    response.validate(&request, child.id(), ReplyKind::Authenticated)?;
    process_identity::authenticate(&owner.socket, child.id(), &configuration.publisher_team_id)?;
    // Do not transfer the actual profile-lock capability before mutual native
    // identity authentication. A challenge/hash is not a file capability.
    remaining(deadline, cancel)?;
    send_lock(&owner.socket, &inherited)?;
    let response: Captured = receive(&mut owner.socket, deadline, cancel)?;
    response.validate(&request, child.id(), ReplyKind::Captured)?;
    process_identity::authenticate(&owner.socket, child.id(), &configuration.publisher_team_id)?;
    if cancel.load(Ordering::Acquire) || child.try_wait().map_err(|_| FAILED)?.is_some() {
        return Err(FAILED.into());
    }
    Ok(owner)
}

/// Reserved process entry, dispatched before activation/profile/UI/MCP work.
/// No handoff target, profile file or dynamic secret is accepted through argv.
#[doc(hidden)]
pub fn main_entry() -> Option<i32> {
    let arguments: Vec<_> = std::env::args_os().skip(1).collect();
    dispatch(&arguments, run)
}
fn dispatch(
    arguments: &[std::ffi::OsString],
    worker: impl FnOnce() -> Result<(), String>,
) -> Option<i32> {
    if !arguments.iter().any(|arg| arg == ENTRY) {
        return None;
    }
    Some(
        if arguments.len() == 1 && arguments[0] == ENTRY && worker().is_ok() {
            0
        } else {
            2
        },
    )
}
fn accept(
    socket: &mut UnixStream,
    parent: u32,
    deadline: Instant,
    cancel: &AtomicBool,
    authenticate: impl FnOnce(&UnixStream, u32) -> Result<(), String>,
) -> Result<Request, String> {
    let request: Request = receive(socket, deadline, cancel)?;
    request.validate(parent)?;
    authenticate(socket, parent)?;
    Ok(request)
}
fn run() -> Result<(), String> {
    let configuration = crate::sparkle::check::compiled()?.ok_or(FAILED)?;
    crate::sparkle::require_supported_macos(
        super::NSProcessInfo::processInfo()
            .operatingSystemVersion()
            .majorVersion,
    )?;
    let fd = unsafe { libc::fcntl(0, libc::F_DUPFD_CLOEXEC, 3) };
    if fd < 0 {
        return Err(FAILED.into());
    }
    let mut socket = unsafe { UnixStream::from_raw_fd(fd) };
    no_sigpipe(&socket)?;
    let cancel = Arc::new(AtomicBool::new(false));
    let deadline = Instant::now() + DEADLINE;
    let parent = u32::try_from(unsafe { libc::getppid() }).map_err(|_| FAILED)?;
    let request = accept(&mut socket, parent, deadline, &cancel, |socket, parent| {
        process_identity::authenticate(socket, parent, &configuration.publisher_team_id)
    })?;
    send(
        &mut socket,
        &Captured {
            kind: ReplyKind::Authenticated,
            schema: 1,
            nonce: request.nonce.clone(),
            owner_pid: std::process::id(),
            checkpoint: request.checkpoint.clone(),
            manifest_sha256: request.manifest_sha256.clone(),
            build: request.build.clone(),
        },
        deadline,
        &cancel,
    )?;
    let lock = receive_lock(&socket, deadline, &cancel)?;
    let profile = UpdateProfile::from_inherited(crate::settings::Locations::user()?, lock)?;
    let executable = std::env::current_exe().map_err(|_| FAILED)?;
    let parent_executable = process_identity::executable(parent)?;
    let copy = checkpoint::owner_copy(&executable, &profile.locations, &request.checkpoint)?;
    let monitor = socket.try_clone().map_err(|_| FAILED)?;
    let monitor_cancel = cancel.clone();
    let watcher = std::thread::Builder::new()
        .name("peerbrush-capture-parent-watch".into())
        .spawn(move || {
            let mut monitor = monitor;
            let mut byte = [0_u8];
            loop {
                if remaining(deadline, &monitor_cancel).is_err() {
                    break;
                }
                let _ = monitor.set_read_timeout(Some(Duration::from_millis(100)));
                match monitor.read(&mut byte) {
                    Err(e) if retry(&e) => continue,
                    _ => break, // EOF or any unsupported request only cancels capture.
                }
            }
            monitor_cancel.store(true, Ordering::Release);
        })
        .map_err(|_| FAILED)?;
    let result = (|| {
        checkpoint::inspect_owner_copy(
            &copy,
            &profile,
            &parent_executable,
            &request.manifest_sha256,
            configuration.clone(),
            cancel.clone(),
        )?;
        if unsafe { libc::getppid() } as u32 != parent {
            return Err(FAILED.into());
        }
        process_identity::authenticate(&socket, parent, &configuration.publisher_team_id)?;
        send(
            &mut socket,
            &Captured {
                kind: ReplyKind::Captured,
                schema: 1,
                nonce: request.nonce,
                owner_pid: std::process::id(),
                checkpoint: request.checkpoint,
                manifest_sha256: request.manifest_sha256,
                build: crate::build_info::json().into(),
            },
            deadline,
            &cancel,
        )?;
        // This process holds real preservation/profile-lock ownership only.
        // No native controller, installer, exit, outcome or recovery authority.
        while remaining(deadline, &cancel).is_ok() && unsafe { libc::getppid() } as u32 == parent {
            std::thread::sleep(Duration::from_millis(50));
        }
        Ok(())
    })();
    cancel.store(true, Ordering::Release);
    let _ = socket.shutdown(std::net::Shutdown::Both);
    let _ = watcher.join();
    result
}

#[cfg(test)]
#[path = "coordinator_tests.rs"]
mod tests;
