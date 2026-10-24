//! Authenticated copied capture/native lifetime. Native startup remains disabled.
use super::{checkpoint, driver, process_identity, Probe};
use crate::{settings::UpdateProfile, sparkle::check::ApprovedCheck};
use objc2::{
    class, define_class, msg_send, rc::Retained, runtime::AnyObject, MainThreadMarker,
    MainThreadOnly,
};
use objc2_foundation::{NSBundle, NSDate, NSObject, NSObjectProtocol};
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
        mpsc, Arc, Condvar, Mutex,
    },
    time::{Duration, Instant},
};

const ENTRY: &str = "--peerbrush-update-capture-owner";
const FAILED: &str = "App update capture owner is unavailable; all projects remain open";
const FRAME_LIMIT: usize = 4096;
const DEADLINE: Duration = Duration::from_secs(600);
#[derive(Clone, Serialize, Deserialize)]
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
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum CheckAction {
    Check,
    Status,
    Cancel,
    Download,
    ObserveFinalProfile,
    PrepareFinalControl,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CheckCommand {
    schema: u32,
    nonce: String,
    checkpoint: String,
    manifest_sha256: String,
    sequence: u64,
    action: CheckAction,
    found_generation: Option<u64>,
    final_profile: Option<checkpoint::FinalProfileBinding>,
}
impl CheckCommand {
    fn new(request: &Request, sequence: u64, action: CheckAction) -> Self {
        Self {
            schema: 1,
            nonce: request.nonce.clone(),
            checkpoint: request.checkpoint.clone(),
            manifest_sha256: request.manifest_sha256.clone(),
            sequence,
            action,
            found_generation: None,
            final_profile: None,
        }
    }
    fn validate(&self, request: &Request, sequence: u64) -> Result<(), String> {
        if self.schema != 1
            || self.nonce != request.nonce
            || self.checkpoint != request.checkpoint
            || self.manifest_sha256 != request.manifest_sha256
            || self.sequence != sequence
            || sequence == 0
            || (sequence == 1 && self.action == CheckAction::Status)
            || (sequence != 1 && self.action == CheckAction::Check)
            || (sequence == 1
                && matches!(
                    self.action,
                    CheckAction::Download
                        | CheckAction::ObserveFinalProfile
                        | CheckAction::PrepareFinalControl
                ))
            || matches!(
                self.action,
                CheckAction::ObserveFinalProfile | CheckAction::PrepareFinalControl
            ) != self.final_profile.is_some()
            || (self.action == CheckAction::Download) != self.found_generation.is_some()
            || self.found_generation == Some(0)
        {
            return Err(FAILED.into());
        }
        if let Some(binding) = &self.final_profile {
            binding.validate()?;
        }
        Ok(())
    }
}
/// Actual SDK callback facts, never installation/health or cancellation proof.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub(super) enum NativeOutcome {
    #[default]
    Unknown,
    Installing {
        application_terminated: bool,
    },
    Installed {
        relaunched: bool,
    },
    Aborted,
    CycleEnded,
}
/// Observation only. A held callback is neither an install permit nor readiness.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct NativeStatus {
    pub(super) phase: crate::sparkle::check::Phase,
    pub(super) version: Option<String>,
    pub(super) busy: bool,
    pub(crate) cancelled: bool,
    pub(crate) quarantine: bool,
    pub(super) native_cycle: bool,
    pub(crate) held_generation: Option<u64>,
    pub(crate) found_generation: Option<u64>,
    pub(crate) download_generation: Option<u64>,
    pub(crate) expected_bytes: Option<u64>,
    pub(crate) received_bytes: u64,
    pub(crate) extraction: Option<f64>,
    pub(super) download_complete: bool,
    pub(super) final_profile: Option<checkpoint::FinalProfileBinding>,
    pub(super) outcome: NativeOutcome,
}
impl NativeStatus {
    fn validate(&self) -> Result<(), String> {
        if self.outcome != NativeOutcome::Unknown
            && (!self.native_cycle || !self.quarantine || self.final_profile.is_some())
        {
            return Err(FAILED.into());
        }
        if matches!(
            self.outcome,
            NativeOutcome::Installing { .. } | NativeOutcome::Installed { .. }
        ) && self.held_generation.is_none()
        {
            return Err(FAILED.into());
        }
        if let Some(binding) = &self.final_profile {
            binding.validate()?;
            if self.cancelled || self.held_generation != Some(binding.ready_generation) {
                return Err(FAILED.into());
            }
        }
        if self.version.as_ref().is_some_and(|version| {
            version.is_empty() || version.len() > 128 || version.chars().any(char::is_control)
        }) || self
            .held_generation
            .is_some_and(|generation| generation == 0 || !self.native_cycle || !self.quarantine)
            || self.found_generation.is_some_and(|generation| {
                generation == 0
                    || !self.native_cycle
                    || self.quarantine
                    || self.cancelled
                    || self.phase != crate::sparkle::check::Phase::Found
                    || self.download_generation.is_some()
            })
            || self
                .download_generation
                .is_some_and(|generation| generation == 0 || !self.native_cycle)
            || (self.download_generation.is_none()
                && (self.expected_bytes.is_some()
                    || self.received_bytes != 0
                    || self.extraction.is_some()
                    || self.download_complete))
            || self
                .extraction
                .is_some_and(|progress| !progress.is_finite() || !(0.0..=1.0).contains(&progress))
        {
            return Err(FAILED.into());
        }
        Ok(())
    }
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CheckReply {
    binding: CheckCommand,
    owner_pid: u32,
    status: NativeStatus,
    final_control: Option<checkpoint::FinalProfileBinding>,
}
impl CheckReply {
    fn validate(
        &self,
        request: &Request,
        child: u32,
        sequence: u64,
        action: CheckAction,
        found_generation: Option<u64>,
        final_profile: Option<&checkpoint::FinalProfileBinding>,
    ) -> Result<(), String> {
        self.binding.validate(request, sequence)?;
        if child <= 1
            || self.owner_pid != child
            || self.binding.action != action
            || self.binding.found_generation != found_generation
            || self.binding.final_profile.as_ref() != final_profile
            || (action == CheckAction::PrepareFinalControl) != self.final_control.is_some()
            || self.final_control.as_ref().is_some_and(|binding| {
                Some(binding) != final_profile
                    || self.status.final_profile.as_ref() != Some(binding)
                    || self.status.held_generation != Some(binding.ready_generation)
                    || self.status.cancelled
            })
        {
            return Err(FAILED.into());
        }
        self.status.validate()
    }
}
fn deliver_check_reply(
    reply: CheckReply,
    observations: &mpsc::SyncSender<Result<NativeStatus, String>>,
    control: &mpsc::SyncSender<Result<checkpoint::FinalProfileBinding, String>>,
) -> Result<bool, String> {
    // This receives an already authenticated/validated frame from our sole reader.
    // Control acknowledgement cannot be coalesced with an older status observation.
    if reply.binding.action == CheckAction::PrepareFinalControl {
        control
            .try_send(Ok(reply.final_control.ok_or(FAILED)?))
            .map_err(|_| FAILED)?;
    }
    match observations.try_send(Ok(reply.status)) {
        Ok(()) | Err(mpsc::TrySendError::Full(_)) => Ok(true),
        Err(mpsc::TrySendError::Disconnected(_)) => Ok(false),
    }
}
enum OwnerRequest {
    Download(u64),
    ObserveFinalProfile(checkpoint::FinalProfileBinding),
    PrepareFinalControl(checkpoint::FinalProfileBinding),
}
pub(super) struct RemoteCheck {
    pub(super) receiver: mpsc::Receiver<Result<NativeStatus, String>>,
    pub(super) final_control_reply: mpsc::Receiver<Result<checkpoint::FinalProfileBinding, String>>,
    cancel: Arc<AtomicBool>,
    requests: mpsc::SyncSender<OwnerRequest>,
    download_requested: AtomicBool,
    final_profile_requested: AtomicBool,
    final_control_requested: AtomicBool,
}
impl RemoteCheck {
    pub(super) fn require_active(&self) -> Result<(), String> {
        if self.cancel.load(Ordering::Acquire) {
            return Err(FAILED.into());
        }
        Ok(())
    }
    pub(super) fn cancel(&self) {
        self.cancel.store(true, Ordering::Release);
    }
    pub(super) fn download(&self, generation: u64) -> Result<(), String> {
        if generation == 0 || self.cancel.load(Ordering::Acquire) {
            return Err(FAILED.into());
        }
        if self
            .download_requested
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return Err("A native download request is already held".into());
        }
        self.requests
            .try_send(OwnerRequest::Download(generation))
            .map_err(|_| FAILED.into())
    }
    pub(super) fn observe_final_profile(
        &self,
        binding: checkpoint::FinalProfileBinding,
    ) -> Result<(), String> {
        binding.validate()?;
        if self.cancel.load(Ordering::Acquire)
            || self
                .final_profile_requested
                .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                .is_err()
        {
            return Err("A final profile observation is already held or cancelled".into());
        }
        self.requests
            .try_send(OwnerRequest::ObserveFinalProfile(binding))
            .map_err(|_| FAILED.into())
    }
    pub(super) fn prepare_final_control(
        &self,
        binding: checkpoint::FinalProfileBinding,
    ) -> Result<(), String> {
        binding.validate()?;
        if self.cancel.load(Ordering::Acquire)
            || self
                .final_control_requested
                .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                .is_err()
        {
            return Err("Final update control is already held or cancelled".into());
        }
        self.requests
            .try_send(OwnerRequest::PrepareFinalControl(binding))
            .map_err(|_| FAILED.into())
    }
    pub(super) fn download_requested(&self) -> bool {
        self.download_requested.load(Ordering::Acquire)
    }
}
impl Drop for RemoteCheck {
    fn drop(&mut self) {
        self.cancel();
    }
}

fn accept_check_command(
    socket: &mut UnixStream,
    request: &Request,
    sequence: u64,
    deadline: Instant,
    cancel: &AtomicBool,
    authenticate: impl FnOnce(&UnixStream) -> Result<(), String>,
) -> Result<CheckCommand, String> {
    let command: CheckCommand = receive(socket, deadline, cancel)?;
    command.validate(request, sequence)?;
    authenticate(socket)?;
    Ok(command)
}
struct NativeCommand {
    action: CheckAction,
    found_generation: Option<u64>,
    final_profile: Option<checkpoint::FinalProfileBinding>,
    reply: mpsc::SyncSender<NativeReply>,
}
struct NativeReply {
    status: NativeStatus,
    final_control: Option<checkpoint::FinalProfileBinding>,
}
fn check_reader(
    mut socket: UnixStream,
    request: Request,
    team: String,
    deadline: Instant,
    cancel: Arc<AtomicBool>,
    commands: mpsc::SyncSender<NativeCommand>,
) {
    let operation = (|| -> Result<(), String> {
        let mut sequence = 1_u64;
        loop {
            let command = accept_check_command(
                &mut socket,
                &request,
                sequence,
                deadline,
                &cancel,
                |channel| process_identity::authenticate(channel, request.parent_pid, &team),
            )?;
            let (reply, result) = mpsc::sync_channel(1);
            commands
                .try_send(NativeCommand {
                    action: command.action,
                    found_generation: command.found_generation,
                    final_profile: command.final_profile.clone(),
                    reply,
                })
                .map_err(|_| FAILED)?;
            let reply = loop {
                remaining(deadline, &cancel)?;
                match result.recv_timeout(Duration::from_millis(100)) {
                    Ok(status) => break status,
                    Err(mpsc::RecvTimeoutError::Timeout) => {}
                    Err(mpsc::RecvTimeoutError::Disconnected) => return Err(FAILED.into()),
                }
            };
            reply.status.validate()?;
            process_identity::authenticate(&socket, request.parent_pid, &team)?;
            send(
                &mut socket,
                &CheckReply {
                    binding: command,
                    owner_pid: std::process::id(),
                    status: reply.status,
                    final_control: reply.final_control,
                },
                deadline,
                &cancel,
            )?;
            sequence = sequence.checked_add(1).ok_or(FAILED)?;
        }
    })();
    let _ = operation;
    // This ends observation only. NativeOwner independently retains every SDK
    // object/callback even when its only reader loses the parent or deadline.
    cancel.store(true, Ordering::Release);
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
    request: Option<Request>,
}
impl Owner {
    pub(super) fn take_exited(&mut self) -> Result<Option<u32>, String> {
        let Some((child, _)) = self.child.as_mut() else {
            return Ok(None);
        };
        match child.try_wait() {
            Ok(None) => return Ok(None),
            Ok(Some(_)) => {}
            Err(_) => {
                // An unqueryable child remains owned by the existing waiter;
                // stop retrying this failed observation without claiming death.
                if let Some((child, reaper)) = self.child.take() {
                    reaper.handoff(child);
                }
                return Err(FAILED.into());
            }
        }
        let (child, reaper) = self.child.take().ok_or(FAILED)?;
        let pid = child.id();
        reaper.handoff(child);
        Ok(Some(pid))
    }
    pub(super) fn authenticate(&self, team: &str) -> Result<(), String> {
        process_identity::authenticate(&self.socket, self.pid()?, team)
    }
    pub(super) fn pid(&self) -> Result<u32, String> {
        self.child
            .as_ref()
            .map(|(child, _)| child.id())
            .filter(|pid| *pid > 1)
            .ok_or_else(|| FAILED.into())
    }
    pub(super) fn check(
        &mut self,
        team: String,
        ctx: &eframe::egui::Context,
    ) -> Result<RemoteCheck, String> {
        // Public configuration cannot activate the unfinished native contract.
        checkpoint::require_captured_handoff()?;
        // Consume the sole reader grant. A clone of the socket is not authority
        // to start another worker competing for the same framed byte stream.
        let request = self.request.take().ok_or(FAILED)?;
        let child = self.child.as_ref().ok_or(FAILED)?.0.id();
        let mut socket = self.socket.try_clone().map_err(|_| FAILED)?;
        let cancel = Arc::new(AtomicBool::new(false));
        let requested_cancel = cancel.clone();
        let (sender, receiver) = mpsc::sync_channel(1);
        let (control_sender, final_control_reply) = mpsc::sync_channel(1);
        let (requests, pending) = mpsc::sync_channel(1);
        let repaint = ctx.clone();
        std::thread::Builder::new()
            .name("peerbrush-native-check-status".into())
            .spawn(move || {
                let result = (|| -> Result<(), String> {
                    let deadline = Instant::now() + DEADLINE;
                    let transport = AtomicBool::new(false);
                    let mut sequence = 1_u64;
                    loop {
                        let (action, generation, final_profile) = if requested_cancel
                            .load(Ordering::Acquire)
                        {
                            (CheckAction::Cancel, None, None)
                        } else if sequence == 1 {
                            (CheckAction::Check, None, None)
                        } else {
                            match pending.try_recv() {
                                Ok(OwnerRequest::Download(generation)) => {
                                    (CheckAction::Download, Some(generation), None)
                                }
                                Ok(OwnerRequest::ObserveFinalProfile(binding)) => {
                                    (CheckAction::ObserveFinalProfile, None, Some(binding))
                                }
                                Ok(OwnerRequest::PrepareFinalControl(binding)) => {
                                    (CheckAction::PrepareFinalControl, None, Some(binding))
                                }
                                Err(mpsc::TryRecvError::Empty) => (CheckAction::Status, None, None),
                                Err(mpsc::TryRecvError::Disconnected) => return Err(FAILED.into()),
                            }
                        };
                        let mut command = CheckCommand::new(&request, sequence, action);
                        command.found_generation = generation;
                        command.final_profile = final_profile.clone();
                        command.validate(&request, sequence)?;
                        process_identity::authenticate(&socket, child, &team)?;
                        send(&mut socket, &command, deadline, &transport)?;
                        let reply: CheckReply = receive(&mut socket, deadline, &transport)?;
                        reply.validate(
                            &request,
                            child,
                            sequence,
                            action,
                            generation,
                            final_profile.as_ref(),
                        )?;
                        process_identity::authenticate(&socket, child, &team)?;
                        if !deliver_check_reply(reply, &sender, &control_sender)? {
                            return Ok(());
                        }
                        repaint.request_repaint();
                        if action == CheckAction::Cancel {
                            return Ok(());
                        }
                        sequence = sequence.checked_add(1).ok_or(FAILED)?;
                        std::thread::sleep(Duration::from_millis(50));
                    }
                })();
                if let Err(error) = result {
                    requested_cancel.store(true, Ordering::Release);
                    let _ = control_sender.try_send(Err(error.clone()));
                    let _ = sender.try_send(Err(error));
                    repaint.request_repaint();
                }
            })
            .map_err(|_| FAILED)?;
        Ok(RemoteCheck {
            receiver,
            final_control_reply,
            cancel,
            requests,
            download_requested: AtomicBool::new(false),
            final_profile_requested: AtomicBool::new(false),
            final_control_requested: AtomicBool::new(false),
        })
    }
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
        request: None,
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
    owner.request = Some(request);
    Ok(owner)
}

// Only this copied process owns its application delegate; the ordinary UI keeps
// winit's delegate. No completion/EOF callback can authorize native termination.
define_class!(
    #[unsafe(super = NSObject)]
    #[thread_kind = MainThreadOnly]
    #[ivars = ()]
    struct NativeOwnerTermination;

    unsafe impl NSObjectProtocol for NativeOwnerTermination {}

    impl NativeOwnerTermination {
        #[unsafe(method(applicationShouldTerminate:))]
        fn should_terminate(&self, _application: &AnyObject) -> usize { 0 }
    }
);

/// Credential-free component assertion only; do not replace the UI delegate.
pub(super) fn assert_native_termination_refusal(marker: MainThreadMarker) {
    let delegate = NativeOwnerTermination::alloc(marker).set_ivars(());
    let delegate: Retained<NativeOwnerTermination> = unsafe { msg_send![super(delegate), init] };
    let application: Retained<AnyObject> =
        unsafe { msg_send![class!(NSApplication), sharedApplication] };
    let reply: usize = unsafe { msg_send![&*delegate, applicationShouldTerminate: &*application] };
    assert_eq!(
        reply, 0,
        "Unknown native outcome cannot permit ordinary owner Quit"
    );
}

/// Main-thread lifetime for the copied native owner. Every check remains behind
/// the independent source refusal; no install or successful retirement API exists.
struct NativeOwner {
    _updater: Probe,
    _profile: UpdateProfile,
    _channel: UnixStream,
    _bundles: checkpoint::OwnerBundles,
    _application: Retained<AnyObject>,
    _termination: Retained<NativeOwnerTermination>,
    run_loop: Retained<AnyObject>,
    checks: std::rc::Rc<driver::State>,
    parent: u32,
    manifest: String,
    configuration: ApprovedCheck,
    cancel: Arc<AtomicBool>,
    check_attempted: bool,
    armed: Option<checkpoint::ArmedCheck>,
    final_profile: Option<checkpoint::FinalProfileBinding>,
    final_control: Option<checkpoint::FinalProfileBinding>,
    parent_exit_recorded: bool,
    outcome_recorded: NativeOutcome,
}
impl NativeOwner {
    fn load(
        bundles: checkpoint::OwnerBundles,
        profile: UpdateProfile,
        channel: UnixStream,
        parent: u32,
        manifest: &str,
        configuration: ApprovedCheck,
        cancel: Arc<AtomicBool>,
    ) -> Result<Self, String> {
        // Public pins/capture replies cannot replace the unfinished surviving
        // native handoff/final exit/outcome/health/recovery contract.
        checkpoint::require_captured_handoff()?;
        let marker = MainThreadMarker::new().ok_or(FAILED)?;
        process_identity::authenticate(&channel, parent, &configuration.publisher_team_id)?;
        let parent_executable = process_identity::executable(parent)?;
        let fresh = checkpoint::inspect_owner_copy(
            &bundles.owner,
            &profile,
            &parent_executable,
            manifest,
            configuration.clone(),
            cancel.clone(),
        )?;
        if fresh != bundles {
            return Err(FAILED.into());
        }
        let running = std::env::current_exe().map_err(|_| FAILED)?;
        let running_bundle = NSBundle::mainBundle();
        let main_bundle = PathBuf::from(running_bundle.bundlePath().to_string());
        bundles.require_running_copy(&running, &main_bundle)?;
        let host = NSBundle::bundleWithPath(&super::NSString::from_str(
            bundles.host.to_str().ok_or(FAILED)?,
        ))
        .ok_or(FAILED)?;
        if Path::new(&host.bundlePath().to_string()) != bundles.host {
            return Err(FAILED.into());
        }
        let same_as_owner: objc2::runtime::Bool =
            unsafe { msg_send![&*host, isEqualTo: &*running_bundle] };
        if same_as_owner.as_bool() {
            return Err(FAILED.into());
        }
        // Both public initializer arguments target the original installed app;
        // the SPUUpdater class must come from the verified copied framework.
        let checks = std::rc::Rc::new(driver::State::default());
        let updater = super::construct_with_bundles(
            &bundles.framework(),
            marker,
            checks.clone(),
            host.clone(),
            host,
        )?;
        let after = checkpoint::inspect_owner_copy(
            &bundles.owner,
            &profile,
            &parent_executable,
            manifest,
            configuration.clone(),
            cancel.clone(),
        )?;
        if after != bundles || process_identity::executable(parent)? != parent_executable {
            return Err(FAILED.into());
        }
        process_identity::authenticate(&channel, parent, &configuration.publisher_team_id)?;
        let application: Retained<AnyObject> =
            unsafe { msg_send![class!(NSApplication), sharedApplication] };
        let existing: Option<Retained<AnyObject>> = unsafe { msg_send![&*application, delegate] };
        if existing.is_some() {
            return Err(FAILED.into());
        }
        let termination = NativeOwnerTermination::alloc(marker).set_ivars(());
        let termination: Retained<NativeOwnerTermination> =
            unsafe { msg_send![super(termination), init] };
        unsafe {
            let accepted: objc2::runtime::Bool =
                msg_send![&*application, setActivationPolicy: 2_isize];
            if !accepted.as_bool() {
                return Err(FAILED.into());
            }
            let _: () = msg_send![&*application, setDelegate: &*termination];
        }
        let run_loop: Retained<AnyObject> = unsafe { msg_send![class!(NSRunLoop), mainRunLoop] };
        Ok(Self {
            _updater: updater,
            _profile: profile,
            _channel: channel,
            _bundles: bundles,
            _application: application,
            _termination: termination,
            run_loop,
            checks,
            parent,
            manifest: manifest.into(),
            configuration,
            cancel,
            check_attempted: false,
            armed: None,
            final_profile: None,
            final_control: None,
            parent_exit_recorded: false,
            outcome_recorded: NativeOutcome::Unknown,
        })
    }
    fn check(&mut self) -> Result<(), String> {
        checkpoint::require_captured_handoff()?;
        if self.check_attempted || self.cancel.load(Ordering::Acquire) {
            return Err(FAILED.into());
        }
        self.check_attempted = true;
        process_identity::authenticate(
            &self._channel,
            self.parent,
            &self.configuration.publisher_team_id,
        )?;
        let parent_executable = process_identity::executable(self.parent)?;
        let fresh = checkpoint::inspect_owner_copy(
            &self._bundles.owner,
            &self._profile,
            &parent_executable,
            &self.manifest,
            self.configuration.clone(),
            self.cancel.clone(),
        )?;
        if fresh != self._bundles {
            return Err(FAILED.into());
        }
        self.checks.begin_in_owner(&self._updater.controller)?;
        // This durable barrier belongs to this copied PID, never the parent.
        self.armed = Some(checkpoint::arm_owner_check(
            &self._bundles.owner,
            &self.manifest,
            &self._profile,
        )?);
        process_identity::authenticate(
            &self._channel,
            self.parent,
            &self.configuration.publisher_team_id,
        )?;
        if self.cancel.load(Ordering::Acquire) {
            return Err(FAILED.into());
        }
        let mut error: *mut super::NSError = std::ptr::null_mut();
        let started: objc2::runtime::Bool =
            unsafe { msg_send![&*self._updater.controller, startUpdater: &mut error] };
        if !started.as_bool() {
            self.armed.as_ref().ok_or(FAILED)?.not_started()?;
            self.checks.native_start_failed();
            return Err(FAILED.into());
        }
        unsafe {
            let _: () = msg_send![&*self._updater.controller, checkForUpdates];
        }
        Ok(())
    }
    fn download(&self, generation: u64) -> Result<(), String> {
        checkpoint::require_captured_handoff()?;
        if !self.check_attempted || self.cancel.load(Ordering::Acquire) {
            return Err(FAILED.into());
        }
        process_identity::authenticate(
            &self._channel,
            self.parent,
            &self.configuration.publisher_team_id,
        )?;
        let parent_executable = process_identity::executable(self.parent)?;
        let fresh = self.armed.as_ref().ok_or(FAILED)?.recheck(
            &self._profile,
            &parent_executable,
            self.configuration.clone(),
            self.cancel.clone(),
        )?;
        if fresh != self._bundles || process_identity::executable(self.parent)? != parent_executable
        {
            return Err(FAILED.into());
        }
        process_identity::authenticate(
            &self._channel,
            self.parent,
            &self.configuration.publisher_team_id,
        )?;
        if self.cancel.load(Ordering::Acquire) {
            return Err(FAILED.into());
        }
        self.checks.download(generation)
    }
    fn observe_final_profile(
        &mut self,
        binding: checkpoint::FinalProfileBinding,
    ) -> Result<(), String> {
        checkpoint::require_captured_handoff()?;
        if self.final_profile.is_some()
            || self.cancel.load(Ordering::Acquire)
            || !self.checks.held_is_current(binding.ready_generation)
        {
            return Err(FAILED.into());
        }
        process_identity::authenticate(
            &self._channel,
            self.parent,
            &self.configuration.publisher_team_id,
        )?;
        self.armed
            .as_ref()
            .ok_or(FAILED)?
            .observe_final_profile(&self._profile, &binding)?;
        process_identity::authenticate(
            &self._channel,
            self.parent,
            &self.configuration.publisher_team_id,
        )?;
        if self.cancel.load(Ordering::Acquire)
            || !self.checks.held_is_current(binding.ready_generation)
        {
            return Err(FAILED.into());
        }
        self.final_profile = Some(binding);
        Ok(())
    }
    fn prepare_final_control(
        &mut self,
        binding: checkpoint::FinalProfileBinding,
    ) -> Result<(), String> {
        checkpoint::require_captured_handoff()?;
        if self.final_control.is_some()
            || self.final_profile.as_ref() != Some(&binding)
            || self.cancel.load(Ordering::Acquire)
            || !self.checks.held_is_current(binding.ready_generation)
        {
            return Err(FAILED.into());
        }
        process_identity::authenticate(
            &self._channel,
            self.parent,
            &self.configuration.publisher_team_id,
        )?;
        self.armed
            .as_ref()
            .ok_or(FAILED)?
            .observe_final_profile(&self._profile, &binding)?;
        self.checks
            .prepare_final_control(binding.ready_generation)?;
        // Retain before any later authentication can fail; no ambiguous retry.
        self.final_control = Some(binding.clone());
        process_identity::authenticate(
            &self._channel,
            self.parent,
            &self.configuration.publisher_team_id,
        )?;
        if self.cancel.load(Ordering::Acquire)
            || !self.checks.final_control_current(binding.ready_generation)
        {
            return Err(FAILED.into());
        }
        Ok(())
    }
    fn status(&self) -> NativeStatus {
        let mut status = self.checks.observed_status();
        status.final_profile = self
            .final_profile
            .as_ref()
            .filter(|binding| {
                self.checks.native_outcome() == NativeOutcome::Unknown
                    && self.checks.held_is_current(binding.ready_generation)
                    && self.final_control.as_ref().is_none_or(|control| {
                        control == *binding
                            && self.checks.final_control_current(binding.ready_generation)
                    })
            })
            .cloned();
        status
    }
    fn retain_unknown_outcome(mut self, commands: mpsc::Receiver<NativeCommand>) -> ! {
        // No verified native outcome/health retirement exists yet. In particular,
        // parent EOF, deadline, NoUpdate and cycle finish cannot release this owner.
        loop {
            // Only kernel reparenting establishes this direct parent's exit.
            // Socket EOF/deadline alone can never stand in for process death.
            let parent = unsafe { libc::getppid() };
            if !self.parent_exit_recorded && parent > 0 && parent as u32 != self.parent {
                self.parent_exit_recorded = true;
                if let Some(armed) = &self.armed {
                    let _ = armed.record_event(
                        &self._profile,
                        checkpoint::NativeEvent::ParentExited,
                        self.checks.held_generation(),
                    );
                }
                self.checks.quarantine();
            }
            let outcome = self.checks.native_outcome();
            if outcome != self.outcome_recorded {
                self.outcome_recorded = outcome;
                if let Some(armed) = &self.armed {
                    if armed
                        .record_event(
                            &self._profile,
                            checkpoint::NativeEvent::Callback { outcome },
                            self.checks.held_generation(),
                        )
                        .is_err()
                    {
                        self.checks.quarantine();
                    }
                }
            }
            if self.cancel.load(Ordering::Acquire) {
                self.checks.quarantine();
            }
            // No blocking socket read/write or channel wait on this native thread.
            // One command per turn leaves Foundation callbacks an opportunity to run.
            match commands.try_recv() {
                Ok(command) => {
                    let mut final_control = None;
                    match command.action {
                        CheckAction::Check => {
                            if self.check().is_err() {
                                self.checks.quarantine();
                            }
                        }
                        CheckAction::Status => {}
                        CheckAction::ObserveFinalProfile => {
                            if command
                                .final_profile
                                .ok_or_else(|| FAILED.to_string())
                                .and_then(|binding| self.observe_final_profile(binding))
                                .is_err()
                            {
                                self.final_profile = None;
                                self.checks.quarantine();
                            }
                        }
                        CheckAction::PrepareFinalControl => {
                            match command
                                .final_profile
                                .ok_or_else(|| FAILED.to_string())
                                .and_then(|binding| {
                                    self.prepare_final_control(binding.clone())
                                        .map(|()| binding)
                                }) {
                                Ok(binding) => final_control = Some(binding),
                                Err(_) => self.checks.quarantine(),
                            }
                        }
                        CheckAction::Cancel => self.checks.cancel(),
                        CheckAction::Download => {
                            if command
                                .found_generation
                                .ok_or_else(|| FAILED.to_string())
                                .and_then(|generation| self.download(generation))
                                .is_err()
                            {
                                self.checks.quarantine();
                            }
                        }
                    }
                    let _ = command.reply.try_send(NativeReply {
                        status: self.status(),
                        final_control,
                    });
                }
                Err(mpsc::TryRecvError::Disconnected) => self.checks.quarantine(),
                Err(mpsc::TryRecvError::Empty) => {}
            }
            objc2::rc::autoreleasepool(|_| {
                let until = NSDate::dateWithTimeIntervalSinceNow(0.05);
                unsafe {
                    let _: () = msg_send![&*self.run_loop, runUntilDate: &*until];
                }
            });
            // Foundation can return early when no source is ready.
            std::thread::sleep(Duration::from_millis(10));
        }
    }
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
    let monitor_request = request.clone();
    let monitor_team = configuration.publisher_team_id.clone();
    let (commands, native_commands) = mpsc::sync_channel(1);
    let watcher = std::thread::Builder::new()
        .name("peerbrush-capture-parent-watch".into())
        .spawn(move || {
            check_reader(
                monitor,
                monitor_request,
                monitor_team,
                deadline,
                monitor_cancel,
                commands,
            )
        })
        .map_err(|_| FAILED)?;
    let result = (|| {
        let bundles = checkpoint::inspect_owner_copy(
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
                nonce: request.nonce.clone(),
                owner_pid: std::process::id(),
                checkpoint: request.checkpoint.clone(),
                manifest_sha256: request.manifest_sha256.clone(),
                build: crate::build_info::json().into(),
            },
            deadline,
            &cancel,
        )?;
        // The independent refusal in load keeps today's authenticated process
        // capture-only. No NativeReady reply, start or installer action is sent.
        if let Ok(native) = NativeOwner::load(
            bundles,
            profile.clone(),
            socket.try_clone().map_err(|_| FAILED)?,
            parent,
            &request.manifest_sha256,
            configuration.clone(),
            cancel.clone(),
        ) {
            native.retain_unknown_outcome(native_commands);
        }
        // Known-unstarted capture still cancels on parent loss. It does not
        // establish absence/cancellation of any previous Sparkle installer.
        while remaining(deadline, &cancel).is_ok() && unsafe { libc::getppid() } as u32 == parent {
            match native_commands.try_recv() {
                Ok(_) | Err(mpsc::TryRecvError::Disconnected) => break,
                Err(mpsc::TryRecvError::Empty) => {}
            }
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
