//! Source-bound, isolated JPEG XL raster operations. Existing workers retain
//! source/revision, history and atomic-publication ownership. No native codec
//! function is called in the application process for file data.
mod native;
mod pipe;
mod process_limits;
pub use native::{Decoded, Info, Options, Pixels, PixelsRef, ENCODED_LIMIT};
use serde::{Deserialize, Serialize};
use std::{
    ffi::OsStr,
    io::{Read, Write},
    path::PathBuf,
    process::{Child, Command, Stdio},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc, Mutex, OnceLock,
    },
    thread,
    time::{Duration, Instant},
};

const MAGIC: &[u8; 8] = b"PBJXL001";
const HEADER_LIMIT: usize = 4096;
const RAW_LIMIT: usize = 256 * 1024 * 1024;
const DEADLINE: Duration = Duration::from_secs(120);
const FAILED: &str = "JPEG XL worker failed; current work was preserved";
const PROTOCOL: &str = "JPEG XL worker response was invalid; current work was preserved";
static ACTIVE: Mutex<()> = Mutex::new(());
static MAIN_EXE: OnceLock<Result<PathBuf, String>> = OnceLock::new();
static FAILED_STOP: AtomicBool = AtomicBool::new(false);
static RETAINED: Mutex<Option<Child>> = Mutex::new(None);

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Operation {
    Inspect,
    Decode,
    Encode,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Kind {
    Ready,
    Result,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum ErrorCode {
    InvalidOrUnsupported,
    UnsupportedProfile,
}
impl ErrorCode {
    fn message(self) -> String {
        match self {
            Self::InvalidOrUnsupported => {
                "JPEG XL is invalid, unsupported or exceeds the codec limits"
            }
            Self::UnsupportedProfile => {
                "JPEG XL ICC profile or output color conversion is unsupported"
            }
        }
        .into()
    }
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    nonce: String,
    build: String,
    operation: Operation,
    info: Option<Info>,
    options: Option<Options>,
    profile_bytes: u32,
    payload_bytes: u64,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Response {
    nonce: String,
    build: String,
    operation: Operation,
    kind: Kind,
    info: Option<Info>,
    profile_bytes: u32,
    payload_bytes: u64,
    error: Option<ErrorCode>,
}
impl Request {
    fn validate(&self) -> Result<(), String> {
        if self.build != build()?
            || self.nonce.len() != 32
            || !self
                .nonce
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err(PROTOCOL.into());
        }
        match self.operation {
            Operation::Inspect | Operation::Decode => {
                if self.info.is_some()
                    || self.options.is_some()
                    || self.profile_bytes != 0
                    || !(2..=ENCODED_LIMIT as u64).contains(&self.payload_bytes)
                {
                    return Err(PROTOCOL.into());
                }
            }
            Operation::Encode => {
                let info = self.info.ok_or(PROTOCOL)?;
                self.options.ok_or(PROTOCOL)?.validate()?;
                if !(1..=crate::color_profile::MAX_PROFILE_BYTES as u32)
                    .contains(&self.profile_bytes)
                    || self.payload_bytes != raw_size(info)? as u64
                {
                    return Err(PROTOCOL.into());
                }
            }
        }
        Ok(())
    }
    fn response(&self, kind: Kind) -> Response {
        Response {
            nonce: self.nonce.clone(),
            build: self.build.clone(),
            operation: self.operation,
            kind,
            info: None,
            profile_bytes: 0,
            payload_bytes: 0,
            error: None,
        }
    }
}
impl Response {
    fn validate(&self, request: &Request) -> Result<(), String> {
        if self.nonce != request.nonce
            || self.build != request.build
            || self.operation != request.operation
        {
            return Err(PROTOCOL.into());
        }
        if self.kind == Kind::Ready || self.error.is_some() {
            if self.info.is_some()
                || self.profile_bytes != 0
                || self.payload_bytes != 0
                || (self.kind == Kind::Ready && self.error.is_some())
            {
                return Err(PROTOCOL.into());
            }
            return Ok(());
        }
        let info = self.info.ok_or(PROTOCOL)?;
        let count = raw_size(info)?;
        match self.operation {
            Operation::Inspect => {
                if self.profile_bytes != 0 || self.payload_bytes != 0 {
                    return Err(PROTOCOL.into());
                }
            }
            Operation::Decode => {
                if !(1..=crate::color_profile::MAX_PROFILE_BYTES as u32)
                    .contains(&self.profile_bytes)
                    || self.payload_bytes != count as u64
                {
                    return Err(PROTOCOL.into());
                }
            }
            Operation::Encode => {
                if Some(info) != request.info
                    || self.profile_bytes != 0
                    || !(2..=ENCODED_LIMIT as u64).contains(&self.payload_bytes)
                {
                    return Err(PROTOCOL.into());
                }
            }
        }
        Ok(())
    }
}
fn build() -> Result<String, String> {
    let value = crate::build_info::json();
    if value.len() > 512 {
        return Err(PROTOCOL.into());
    }
    let record: serde_json::Value = serde_json::from_str(value).map_err(|_| PROTOCOL)?;
    if record["format"] != "PeerBrushBuildInfoV1"
        || record["product"] != "PeerBrush"
        || record["version"] != env!("CARGO_PKG_VERSION")
    {
        return Err(PROTOCOL.into());
    }
    Ok(value.into())
}
fn raw_size(info: Info) -> Result<usize, String> {
    crate::raster::check_size(info.width, info.height)?;
    if !matches!(info.bit_depth, 8 | 16) {
        return Err(PROTOCOL.into());
    }
    (info.width as usize)
        .checked_mul(info.height as usize)
        .and_then(|n| n.checked_mul(4))
        .and_then(|n| n.checked_mul(usize::from(info.bit_depth / 8)))
        .filter(|n| *n <= RAW_LIMIT)
        .ok_or_else(|| PROTOCOL.into())
}
fn header<T: Serialize>(output: &mut impl Write, value: &T) -> Result<(), String> {
    let bytes = serde_json::to_vec(value).map_err(|_| PROTOCOL)?;
    if bytes.len() > HEADER_LIMIT {
        return Err(PROTOCOL.into());
    }
    write(output, MAGIC)?;
    write(output, &(bytes.len() as u32).to_le_bytes())?;
    write(output, &bytes)?;
    output.flush().map_err(|_| FAILED.into())
}
fn read_header<T: for<'a> Deserialize<'a>>(input: &mut impl Read) -> Result<T, String> {
    let mut magic = [0; 8];
    read(input, &mut magic)?;
    if &magic != MAGIC {
        return Err(PROTOCOL.into());
    }
    read_header_body(input)
}
fn read_header_body<T: for<'a> Deserialize<'a>>(input: &mut impl Read) -> Result<T, String> {
    let mut size = [0; 4];
    read(input, &mut size)?;
    let size = u32::from_le_bytes(size) as usize;
    if size == 0 || size > HEADER_LIMIT {
        return Err(PROTOCOL.into());
    }
    serde_json::from_slice(&read_bytes(input, size)?).map_err(|_| PROTOCOL.into())
}
fn read(input: &mut impl Read, mut bytes: &mut [u8]) -> Result<(), String> {
    while !bytes.is_empty() {
        let count = bytes.len().min(65536);
        input
            .read_exact(&mut bytes[..count])
            .map_err(|_| PROTOCOL)?;
        bytes = &mut bytes[count..];
    }
    Ok(())
}
fn write(output: &mut impl Write, bytes: &[u8]) -> Result<(), String> {
    for chunk in bytes.chunks(65536) {
        output.write_all(chunk).map_err(|_| FAILED)?;
    }
    Ok(())
}
fn read_bytes(input: &mut impl Read, size: usize) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::new();
    bytes.try_reserve_exact(size).map_err(|_| FAILED)?;
    bytes.resize(size, 0);
    read(input, &mut bytes)?;
    Ok(bytes)
}
fn read_pixels(input: &mut impl Read, info: Info) -> Result<Pixels, String> {
    let size = raw_size(info)?;
    if info.bit_depth == 8 {
        return read_bytes(input, size).map(Pixels::Eight);
    }
    let mut words = Vec::new();
    words.try_reserve_exact(size / 2).map_err(|_| FAILED)?;
    words.resize(size / 2, 0_u16);
    read(input, bytemuck::cast_slice_mut(&mut words))?;
    Ok(Pixels::Sixteen(words))
}
fn eof(input: &mut impl Read) -> Result<(), String> {
    let mut extra = [0];
    if input.read(&mut extra).map_err(|_| PROTOCOL)? != 0 {
        return Err(PROTOCOL.into());
    }
    Ok(())
}
fn executable() -> Result<PathBuf, String> {
    if let Some(path) = MAIN_EXE.get() {
        return path.clone();
    }
    // Only Cargo's explicit development test layout may select a sibling main.
    // The fresh main must still acknowledge the exact source/version before
    // any image/profile bytes are transferred. No PATH/env override exists.
    let test = std::env::current_exe()
        .map_err(|_| FAILED)?
        .canonicalize()
        .map_err(|_| FAILED)?;
    let deps = test.parent().ok_or(FAILED)?;
    let profile = deps.parent().ok_or(FAILED)?;
    if deps.file_name() != Some(OsStr::new("deps"))
        || !matches!(
            profile.file_name().and_then(OsStr::to_str),
            Some("debug" | "release")
        )
    {
        return Err("JPEG XL requires the PeerBrush application worker entry point".into());
    }
    let record: serde_json::Value = serde_json::from_str(&build()?).map_err(|_| FAILED)?;
    if record["commit"].as_str().is_none_or(|sha| {
        sha.len() != 40
            || !sha
                .bytes()
                .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
    }) {
        return Err("Build a source-matched PeerBrush main executable before codec tests".into());
    }
    let path = profile.join(format!("peerbrush{}", std::env::consts::EXE_SUFFIX));
    if !path
        .symlink_metadata()
        .is_ok_and(|metadata| metadata.is_file() && !metadata.file_type().is_symlink())
    {
        return Err("Build a source-matched PeerBrush main executable before codec tests".into());
    }
    path.canonicalize().map_err(|_| FAILED.into())
}

/// Must be the first action in the real main, before preferences/UI/MCP setup.
#[doc(hidden)]
pub fn main_entry() -> Option<i32> {
    let arguments: Vec<_> = std::env::args_os().skip(1).collect();
    if arguments.first().is_some_and(|arg| arg == "--jxl-worker") {
        return Some(if arguments.len() == 1 && worker().is_ok() {
            0
        } else {
            2
        });
    }
    let path = std::env::current_exe()
        .and_then(|path| path.canonicalize())
        .map_err(|_| FAILED.into());
    let _ = MAIN_EXE.set(path);
    None
}
pub fn signature(bytes: &[u8]) -> bool {
    bytes.starts_with(&[0xff, 0x0a]) || bytes.starts_with(b"\0\0\0\x0cJXL \x0d\x0a\x87\x0a")
}
enum Input<'a> {
    Encoded(&'a [u8]),
    Pixels(PixelsRef<'a>, &'a [u8]),
}
impl Input<'_> {
    fn write(self, output: &mut impl Write) -> Result<(), String> {
        match self {
            Self::Encoded(bytes) => write(output, bytes),
            Self::Pixels(pixels, profile) => {
                write(output, profile)?;
                match pixels {
                    PixelsRef::Eight(bytes) => write(output, bytes),
                    PixelsRef::Sixteen(words) => write(output, bytemuck::cast_slice(words)),
                }
            }
        }
    }
}
struct Output {
    info: Info,
    profile: Vec<u8>,
    pixels: Pixels,
    encoded: Vec<u8>,
}
fn read_output(
    input: &mut impl Read,
    request: &Request,
    #[cfg(test)] started: Option<&std::sync::atomic::AtomicBool>,
) -> Result<Output, String> {
    let result: Response = read_header(input)?;
    result.validate(request)?;
    if result.kind != Kind::Result {
        return Err(PROTOCOL.into());
    }
    if let Some(error) = result.error {
        eof(input)?;
        return Err(error.message());
    }
    let info = result.info.ok_or(PROTOCOL)?;
    let profile = read_bytes(input, result.profile_bytes as usize)?;
    if request.operation == Operation::Decode {
        crate::color_profile::supported(&profile)
            .map_err(|_| ErrorCode::UnsupportedProfile.message())?;
    }
    #[cfg(test)]
    if let Some(started) = started {
        started.store(true, std::sync::atomic::Ordering::SeqCst);
    }
    let pixels = if request.operation == Operation::Decode {
        read_pixels(input, info)?
    } else {
        Pixels::Eight(Vec::new())
    };
    let encoded = if request.operation == Operation::Encode {
        read_bytes(input, result.payload_bytes as usize)?
    } else {
        Vec::new()
    };
    if request.operation == Operation::Encode && !signature(&encoded) {
        return Err(PROTOCOL.into());
    }
    eof(input)?;
    Ok(Output {
        info,
        profile,
        pixels,
        encoded,
    })
}
struct ChildGuard {
    child: Option<Child>,
    done: bool,
    stop: Arc<AtomicBool>,
    _limits: Option<process_limits::Parent>,
    #[cfg(test)]
    primary_kill_failed: bool,
    #[cfg(test)]
    kill_calls: usize,
}
impl ChildGuard {
    fn retain(&mut self) -> Result<(), String> {
        FAILED_STOP.store(true, Ordering::SeqCst);
        // ACTIVE serializes this sole slot. Future calls reject before
        // spawning and may only try to reap this retained owner.
        *RETAINED.lock().unwrap_or_else(|error| error.into_inner()) = self.child.take();
        Err("JPEG XL worker could not be stopped; further codec work is disabled".into())
    }
    fn stop(&mut self) -> Result<(), String> {
        self.stop.store(true, Ordering::SeqCst);
        if !self.done {
            let Some(child) = self.child.as_mut() else {
                return Ok(());
            };
            match child.try_wait() {
                Ok(Some(_)) => {
                    self.done = true;
                    return Ok(());
                }
                Ok(None) => {}
                Err(_) => return self.retain(),
            }
            #[cfg(test)]
            {
                self.kill_calls += 1;
            }
            #[cfg(test)]
            let killed = if self.primary_kill_failed {
                false
            } else {
                child.kill().is_ok()
            };
            #[cfg(not(test))]
            let killed = child.kill().is_ok();
            if !killed {
                #[cfg(windows)]
                if let Some(limits) = self._limits.take() {
                    limits.terminate();
                    drop(limits);
                }
                #[cfg(unix)]
                {
                    // Retry the owned Child API only. A raw PID fallback
                    // after reaping could target an unrelated reused PID.
                    #[cfg(test)]
                    {
                        self.kill_calls += 1;
                    }
                    let _ = child.kill();
                }
            }
            // No blocking wait: even an OS stop failure cannot strand scoped
            // pipe threads. They independently observe the stop flag above.
            let until = Instant::now() + Duration::from_secs(3);
            loop {
                let status = child.try_wait();
                if matches!(status, Ok(Some(_))) {
                    self.done = true;
                    break;
                }
                if status.is_err() || Instant::now() >= until {
                    return self.retain();
                }
                thread::sleep(Duration::from_millis(10));
            }
        }
        Ok(())
    }
}
impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}
struct Runtime {
    timeout: Duration,
    #[cfg(test)]
    command: Option<Command>,
    #[cfg(test)]
    test_prefix: bool,
    #[cfg(test)]
    response_started: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
    #[cfg(test)]
    primary_kill_failed: bool,
    #[cfg(test)]
    input_written: Option<Arc<std::sync::atomic::AtomicUsize>>,
    #[cfg(test)]
    stall_writer: bool,
}
impl Default for Runtime {
    fn default() -> Self {
        #[cfg(test)]
        return tests::runtime("", 8, DEADLINE, None);
        #[cfg(not(test))]
        Self {
            timeout: DEADLINE,
            #[cfg(test)]
            command: None,
            #[cfg(test)]
            test_prefix: false,
            #[cfg(test)]
            response_started: None,
            #[cfg(test)]
            primary_kill_failed: false,
            #[cfg(test)]
            input_written: None,
            #[cfg(test)]
            stall_writer: false,
        }
    }
}
enum Event {
    Ready,
    Result(Result<Output, String>),
}
fn cleanup_failed() -> bool {
    if !FAILED_STOP.load(Ordering::SeqCst) {
        return false;
    }
    let mut retained = RETAINED.lock().unwrap_or_else(|error| error.into_inner());
    if retained
        .as_mut()
        .is_some_and(|child| matches!(child.try_wait(), Ok(Some(_))))
    {
        retained.take();
    }
    true
}
fn invoke(
    request: Request,
    body: Input<'_>,
    check: &dyn Fn() -> Result<(), String>,
    mut runtime: Runtime,
) -> Result<Output, String> {
    check()?;
    request.validate()?;
    if cleanup_failed() {
        return Err("JPEG XL worker cleanup failed; further codec work is disabled".into());
    }
    if !cfg!(target_endian = "little") {
        return Err("JPEG XL worker transport requires a supported little-endian platform".into());
    }
    let started = Instant::now();
    let _active = loop {
        check()?;
        match ACTIVE.try_lock() {
            Ok(guard) => break guard,
            Err(std::sync::TryLockError::Poisoned(_)) => return Err(FAILED.into()),
            Err(std::sync::TryLockError::WouldBlock) => {}
        }
        if started.elapsed() >= runtime.timeout {
            return Err("JPEG XL worker queue timed out; current work was preserved".into());
        }
        thread::sleep(Duration::from_millis(10));
    };
    if cleanup_failed() {
        return Err("JPEG XL worker cleanup failed; further codec work is disabled".into());
    }
    #[cfg(test)]
    let custom = runtime.command.take();
    #[cfg(not(test))]
    let custom: Option<Command> = None;
    let mut command = if let Some(command) = custom {
        command
    } else {
        let executable = executable()?;
        let mut command = Command::new(&executable);
        command.current_dir(executable.parent().ok_or(FAILED)?);
        command.arg("--jxl-worker");
        command.env_clear();
        #[cfg(windows)]
        for key in ["SystemRoot", "WINDIR"] {
            if let Some(value) = std::env::var_os(key) {
                command.env(key, value);
            }
        }
        command
    };
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000);
    }
    let child = command.spawn().map_err(|_| FAILED)?;
    // The guard is inside the scope: it stops pumps and attempts child cleanup
    // before scoped threads join on every return/unwind, including kill error.
    thread::scope(|scope| {
        let stop = Arc::new(AtomicBool::new(false));
        let mut guard = ChildGuard {
            child: Some(child),
            done: false,
            stop: stop.clone(),
            _limits: None,
            #[cfg(test)]
            primary_kill_failed: runtime.primary_kill_failed,
            #[cfg(test)]
            kill_calls: 0,
        };
        guard._limits = Some(process_limits::Parent::new(
            guard.child.as_ref().ok_or(FAILED)?,
        )?);
        let mut input = pipe::Pipe::writer(
            guard
                .child
                .as_mut()
                .ok_or(FAILED)?
                .stdin
                .take()
                .ok_or(FAILED)?,
            stop.clone(),
        )
        .map_err(|_| FAILED)?;
        let mut output = pipe::Pipe::reader(
            guard
                .child
                .as_mut()
                .ok_or(FAILED)?
                .stdout
                .take()
                .ok_or(FAILED)?,
            stop,
        )
        .map_err(|_| FAILED)?;
        header(&mut input, &request)?; // Metadata only, less than one pipe buffer.
        #[cfg(test)]
        {
            input.progress = runtime.input_written.take();
        }
        let (tx, rx) = mpsc::sync_channel(2);
        let selected = request.clone();
        #[cfg(test)]
        let response_started = runtime.response_started.take();
        let reader = scope.spawn(move || {
            let result = (|| {
                #[cfg(test)]
                let ready: Response = if runtime.test_prefix {
                    read_test_ready(&mut output)?
                } else {
                    read_header(&mut output)?
                };
                #[cfg(not(test))]
                let ready: Response = read_header(&mut output)?;
                ready.validate(&selected)?;
                if ready.kind != Kind::Ready {
                    return Err(PROTOCOL.into());
                }
                tx.send(Event::Ready).map_err(|_| FAILED)?;
                read_output(
                    &mut output,
                    &selected,
                    #[cfg(test)]
                    response_started.as_deref(),
                )
            })();
            let _ = tx.send(Event::Result(result));
        });
        let mut input = Some(input);
        let mut body = Some(body);
        let mut writer = None;
        let mut result = None;
        let operation = (|| loop {
            check()?;
            if started.elapsed() >= runtime.timeout {
                return Err("JPEG XL worker timed out; current work was preserved".into());
            }
            loop {
                let event = match rx.try_recv() {
                    Ok(event) => event,
                    Err(mpsc::TryRecvError::Empty) => break,
                    Err(mpsc::TryRecvError::Disconnected) if result.is_some() => break,
                    Err(mpsc::TryRecvError::Disconnected) => return Err(FAILED.into()),
                };
                match event {
                    Event::Ready => {
                        let mut input = input.take().ok_or(PROTOCOL)?;
                        let body = body.take().ok_or(PROTOCOL)?;
                        #[cfg(test)]
                        let writer_stop = guard.stop.clone();
                        writer = Some(scope.spawn(move || {
                            #[cfg(test)]
                            if runtime.stall_writer {
                                while !writer_stop.load(Ordering::SeqCst) {
                                    thread::sleep(Duration::from_millis(1));
                                }
                                return Err(FAILED.into());
                            }
                            body.write(&mut input)
                        }));
                    }
                    Event::Result(output) => {
                        result = Some(output?);
                    }
                }
            }
            let status = guard
                .child
                .as_mut()
                .ok_or(FAILED)?
                .try_wait()
                .map_err(|_| FAILED)?;
            if let Some(status) = status {
                guard.done = true;
                if !status.success() {
                    return Err(FAILED.into());
                }
                if let Some(result) = result.take() {
                    check()?;
                    return Ok(result);
                }
            } else if let Err(error) = process_limits::check(guard.child.as_ref().ok_or(FAILED)?) {
                // A process may exit between try_wait and the macOS sample.
                let exited = guard
                    .child
                    .as_mut()
                    .ok_or(FAILED)?
                    .try_wait()
                    .map_err(|_| FAILED)?
                    .is_some();
                if exited {
                    guard.done = true;
                }
                if error.rejects(exited) {
                    return Err(error.message());
                }
            }
            thread::sleep(Duration::from_millis(10));
        })();
        // Also stop on accepted exit before joining. A malformed child must
        // not strand a writer via a surviving inherited pipe handle. A valid
        // child requires body EOF, so its writer has already closed stdin.
        guard.stop.store(true, Ordering::SeqCst);
        drop(input);
        let cleanup = if operation.is_err() {
            guard.stop()
        } else {
            Ok(())
        };
        let written = writer
            .map(|writer| {
                writer
                    .join()
                    .map_err(|_| FAILED.to_owned())
                    .and_then(|result| result)
            })
            .unwrap_or_else(|| Err(PROTOCOL.into()));
        let read = reader.join().map_err(|_| FAILED);
        cleanup?;
        operation.and_then(|result| {
            written?;
            read?;
            Ok(result)
        })
    })
}
fn request(
    operation: Operation,
    info: Option<Info>,
    options: Option<Options>,
    profile: usize,
    payload: usize,
) -> Result<Request, String> {
    Ok(Request {
        nonce: uuid::Uuid::new_v4().simple().to_string(),
        build: build()?,
        operation,
        info,
        options,
        profile_bytes: u32::try_from(profile).map_err(|_| PROTOCOL)?,
        payload_bytes: payload as u64,
    })
}
pub fn inspect(bytes: &[u8], check: &dyn Fn() -> Result<(), String>) -> Result<Info, String> {
    invoke(
        request(Operation::Inspect, None, None, 0, bytes.len())?,
        Input::Encoded(bytes),
        check,
        Runtime::default(),
    )
    .map(|result| result.info)
}
pub fn decode(bytes: &[u8], check: &dyn Fn() -> Result<(), String>) -> Result<Decoded, String> {
    let result = invoke(
        request(Operation::Decode, None, None, 0, bytes.len())?,
        Input::Encoded(bytes),
        check,
        Runtime::default(),
    )?;
    Ok(Decoded {
        info: result.info,
        pixels: result.pixels,
        icc_profile: result.profile,
    })
}
pub fn encode(
    width: u32,
    height: u32,
    pixels: PixelsRef<'_>,
    profile: Option<&[u8]>,
    options: Options,
    check: &dyn Fn() -> Result<(), String>,
) -> Result<Vec<u8>, String> {
    check()?;
    options.validate()?;
    let (depth, size) = match pixels {
        PixelsRef::Eight(bytes) => (8, bytes.len()),
        PixelsRef::Sixteen(words) => (16, std::mem::size_of_val(words)),
    };
    let info = Info {
        width,
        height,
        bit_depth: depth,
    };
    if raw_size(info)? != size {
        return Err("JPEG XL RGBA sample count differs from its dimensions".into());
    }
    let profile = profile.unwrap_or_else(|| crate::color_profile::srgb_profile());
    crate::color_profile::supported(profile)
        .map_err(|_| ErrorCode::UnsupportedProfile.message())?;
    invoke(
        request(
            Operation::Encode,
            Some(info),
            Some(options),
            profile.len(),
            size,
        )?,
        Input::Pixels(pixels, profile),
        check,
        Runtime::default(),
    )
    .map(|result| result.encoded)
}

fn worker() -> Result<(), String> {
    process_limits::worker()?;
    if !cfg!(target_endian = "little") {
        return Err(FAILED.into());
    }
    let mut input = std::io::stdin().lock();
    let mut output = std::io::stdout().lock();
    let request: Request = read_header(&mut input)?;
    request.validate()?;
    let ready = request.response(Kind::Ready);
    #[cfg(test)]
    let ready = tests::ready_response(ready);
    header(&mut output, &ready)?;
    #[cfg(test)]
    if tests::before_body(&request, &mut output)? {
        return Ok(());
    }
    let profile = read_bytes(&mut input, request.profile_bytes as usize)?;
    let mut pixels = None;
    let encoded = if request.operation == Operation::Encode {
        crate::color_profile::supported(&profile)?;
        pixels = Some(read_pixels(&mut input, request.info.ok_or(PROTOCOL)?)?);
        Vec::new()
    } else {
        read_bytes(&mut input, request.payload_bytes as usize)?
    };
    eof(&mut input)?;
    #[cfg(test)]
    if test_fault(&request, &mut output)? {
        return Ok(());
    }
    let mut result = request.response(Kind::Result);
    let mut returned = None;
    let mut image = None;
    let operation = match request.operation {
        Operation::Inspect => {
            native::inspect(&encoded, &|| Ok(())).map(|info| result.info = Some(info))
        }
        Operation::Decode => native::decode(&encoded, &|| Ok(())).map(|decoded| {
            result.info = Some(decoded.info);
            result.profile_bytes = decoded.icc_profile.len() as u32;
            result.payload_bytes = raw_size(decoded.info).unwrap_or(0) as u64;
            image = Some(decoded);
        }),
        Operation::Encode => {
            let info = request.info.ok_or(PROTOCOL)?;
            let pixels = match pixels.as_ref().ok_or(PROTOCOL)? {
                Pixels::Eight(bytes) => PixelsRef::Eight(bytes),
                Pixels::Sixteen(words) => PixelsRef::Sixteen(words),
            };
            native::encode(
                info.width,
                info.height,
                pixels,
                Some(&profile),
                request.options.ok_or(PROTOCOL)?,
                &|| Ok(()),
            )
            .map(|bytes| {
                result.info = Some(info);
                result.payload_bytes = bytes.len() as u64;
                returned = Some(bytes);
            })
        }
    };
    if let Err(error) = operation {
        result.info = None;
        result.profile_bytes = 0;
        result.payload_bytes = 0;
        result.error = Some(
            if error.to_ascii_lowercase().contains("profile") || error.contains("ICC") {
                ErrorCode::UnsupportedProfile
            } else {
                ErrorCode::InvalidOrUnsupported
            },
        );
    }
    result.validate(&request)?;
    header(&mut output, &result)?;
    if let Some(decoded) = image {
        write(&mut output, &decoded.icc_profile)?;
        match decoded.pixels {
            Pixels::Eight(bytes) => write(&mut output, &bytes)?,
            Pixels::Sixteen(words) => write(&mut output, bytemuck::cast_slice(&words))?,
        }
    }
    if let Some(bytes) = returned {
        write(&mut output, &bytes)?;
    }
    output.flush().map_err(|_| FAILED.into())
}

#[cfg(test)]
mod tests;
#[cfg(test)]
use tests::{read_test_ready, test_fault};
