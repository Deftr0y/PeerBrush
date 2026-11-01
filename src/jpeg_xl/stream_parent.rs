//! Session-owned streaming worker transport. A completed owner is deliberately
//! not destination publication or decoded/current-scene fidelity authority.
use super::{
    stream_protocol::{self as wire, Configuration, Direction, Frame, Message, State},
    *,
};
use crate::{
    large_document::{
        segments::{SegmentEvent, SegmentLimits},
        ScratchBytes,
    },
    production_export::staged_rows::StagedNativeRows,
    workspace::source_runtime::{SessionJxlOutput, SessionSegmentLog},
};
use ring::digest::{Context, SHA256};
use std::sync::atomic::AtomicBool;
const CHILD_BYTES: usize = 96 * 1024 * 1024;
const PARENT_BYTES: usize = 32 * 1024 * 1024;
const WINDOW_BYTES: usize = 32 * 1024 * 1024 - 4 * wire::BUFFER_BYTES;
const NATIVE_BYTES: usize = 512 * 1024 * 1024;
type Result<T> = std::result::Result<T, String>;

pub(crate) struct Limits {
    pub output_bytes: u64,
    pub log_bytes: u64,
    pub records: u32,
    pub input_bytes: u64,
    pub deadline_ms: u32,
}
pub(crate) struct EncodedJxl<'scene, 'output, 'session> {
    output: SessionJxlOutput<'session>,
    input: StagedNativeRows<'scene, 'output, 'session>,
    configuration: Configuration,
    binding: [u8; 32],
    failed: AtomicBool,
}
impl EncodedJxl<'_, '_, '_> {
    pub(crate) fn bytes(&self) -> u64 {
        self.output.bytes().payload_length()
    }
    pub(crate) fn sha256(&self) -> [u8; 32] {
        self.output.bytes().payload_sha256()
    }
    pub(crate) fn source_read_bytes(&self) -> u64 {
        self.input.read_used()
    }
    pub(crate) fn recheck(&self, check: &dyn Fn() -> Result<()>) -> Result<()> {
        let result = (|| {
            if self.failed.load(Ordering::Acquire) {
                return Err("Streaming JXL owner is failed".into());
            }
            self.input.recheck(check)?;
            self.output.check_session(self.input.session())?;
            if self.output.bytes().binding() != self.binding
                || self.output.bytes().role() != crate::large_document::ByteRole::CompatibleJxlMode1
                || self.output.bytes().payload_length() > self.configuration.max_output_bytes
            {
                return Err("Streaming JXL output binding differs".into());
            }
            check()
        })();
        if result.is_err() {
            self.failed.store(true, Ordering::Release);
        }
        result
    }
    /// The actual owner and both namespaces remain borrowed through every copy.
    pub(crate) fn copy_range(
        &self,
        start: u64,
        out: &mut ScratchBytes,
        check: &dyn Fn() -> Result<()>,
    ) -> Result<()> {
        if self.failed.load(Ordering::Acquire) {
            return Err("Streaming JXL owner is failed".into());
        }
        let result = (|| {
            self.input.check(check)?;
            self.output.check_session(self.input.session())?;
            if !out.uses_budget(&self.input.session().resident()) {
                return Err("Streaming JXL output buffer belongs to another pool".into());
            }
            let mut failure = None;
            self.output
                .bytes()
                .read_range_into(start, out, &mut || {
                    self.input
                        .check(check)
                        .and_then(|_| self.output.check())
                        .map_err(|e| {
                            failure = Some(e);
                            crate::large_document::Error::Cancelled
                        })
                })
                .map_err(|e| failure.unwrap_or_else(|| e.to_string()))?;
            self.input.check(check)?;
            self.output.check()
        })();
        if result.is_err() {
            self.failed.store(true, Ordering::Release);
        }
        result
    }
    pub(crate) fn retire(self, cleanup: &mut dyn FnMut() -> Result<()>) -> Result<()> {
        self.output.retire(cleanup)?;
        self.input.retire(cleanup)
    }
}
struct Buffers {
    window: ScratchBytes,
    io: ScratchBytes,
    segment: ScratchBytes,
    row: Option<ScratchBytes>,
}
impl Buffers {
    fn new(input: &StagedNativeRows<'_, '_, '_>) -> Result<Self> {
        let pool = input.session().resident();
        Ok(Self {
            window: ScratchBytes::new(&pool, WINDOW_BYTES, WINDOW_BYTES)
                .map_err(|e| e.to_string())?,
            io: ScratchBytes::new(&pool, wire::BUFFER_BYTES, wire::BUFFER_BYTES)
                .map_err(|e| e.to_string())?,
            segment: ScratchBytes::new(&pool, wire::BUFFER_BYTES, wire::BUFFER_BYTES)
                .map_err(|e| e.to_string())?,
            row: None,
        })
    }
}
fn resources(input: &StagedNativeRows<'_, '_, '_>) -> Result<ChildResources> {
    let pool = input.session().resident();
    let headers = pool
        .reserve_bytes(65536, 65536)
        .map_err(|e| e.to_string())?;
    let mut charges = Vec::new();
    charges
        .try_reserve_exact(12)
        .map_err(|_| "Streaming worker reservation headers refused")?;
    for mut bytes in [NATIVE_BYTES, CHILD_BYTES, input.profile().len()] {
        while bytes != 0 {
            let take = bytes.min(64 * 1024 * 1024);
            charges.push(
                pool.reserve_bytes(take, 64 * 1024 * 1024)
                    .map_err(|e| e.to_string())?,
            );
            bytes -= take;
        }
    }
    Ok(ChildResources {
        charges,
        _headers: headers,
    })
}
fn configuration(input: &StagedNativeRows<'_, '_, '_>, limits: &Limits) -> Result<Configuration> {
    let mut output = Context::new(&SHA256);
    output.update(b"PeerBrushJxlOutputV3\0");
    output.update(&input.binding());
    let configuration = Configuration {
        info: Info {
            width: input.output().width,
            height: input.output().height,
            bit_depth: input.output().native_source_depth,
        },
        options: input.options(),
        profile_bytes: input
            .profile()
            .len()
            .try_into()
            .map_err(|_| "Streaming profile length overflow")?,
        raw_bytes: input.raw_bytes(),
        max_input_bytes: limits.input_bytes,
        max_source_read_bytes: input.read_limit(),
        max_output_bytes: limits.output_bytes,
        max_log_bytes: limits.log_bytes,
        max_records: limits.records,
        max_held_bytes: CHILD_BYTES as u32,
        max_held_buffers: wire::MAX_HELD_BUFFERS,
        max_elapsed_ms: limits.deadline_ms,
        source_binding: input.binding(),
        output_binding: output.finish().as_ref().try_into().unwrap(),
        raw_sha256: input.raw_sha256(),
        profile_sha256: input.profile_sha256(),
        build_sha256: ring::digest::digest(&SHA256, build()?.as_bytes())
            .as_ref()
            .try_into()
            .unwrap(),
    };
    configuration.validate()?;
    Ok(configuration)
}
fn fill_rectangle(
    input: &StagedNativeRows<'_, '_, '_>,
    rectangle: wire::Rectangle,
    buffers: &mut Buffers,
    check: &dyn Fn() -> Result<()>,
) -> Result<(usize, [u8; 32])> {
    input.check(check)?;
    let native = usize::from(input.output().native_source_depth / 8);
    let pixel = 4 * native;
    let count = rectangle.width as usize * rectangle.height as usize * pixel;
    let row = rectangle.width as usize * pixel;
    if count > buffers.window.len() || row > 2048 * 8 {
        return Err("Streaming rectangle exceeds parent buffer admission".into());
    }
    if buffers.row.as_ref().is_none_or(|value| value.len() != row) {
        drop(buffers.row.take());
        buffers.row = Some(
            ScratchBytes::new(&input.session().resident(), row, 2048 * 8)
                .map_err(|e| e.to_string())?,
        );
    }
    // Three64KiB owners include the temporary verifier allocated inside each
    // uncached range read. The short native row and full window stay admitted.
    if buffers.window.len() + 3 * wire::BUFFER_BYTES + row > PARENT_BYTES {
        return Err("Streaming parent callback envelope exceeded".into());
    }
    let mut hash = Context::new(&SHA256);
    let output_width = u64::from(input.output().width);
    for y in 0..rectangle.height {
        let at =
            (u64::from(rectangle.y + y) * output_width + u64::from(rectangle.x)) * pixel as u64;
        let scratch = buffers.row.as_mut().ok_or("Streaming row buffer missing")?;
        input.copy_range(at, scratch, check)?;
        let start = y as usize * row;
        buffers.window[start..start + row].copy_from_slice(scratch);
        hash.update(scratch);
    }
    input.check(check)?;
    Ok((count, hash.finish().as_ref().try_into().unwrap()))
}
#[allow(clippy::too_many_arguments)]
fn exchange<'scene, 'output, 'session>(
    input: StagedNativeRows<'scene, 'output, 'session>,
    mut log: SessionSegmentLog<'session>,
    configuration: Configuration,
    nonce: [u8; 16],
    mut stdin: pipe::Pipe<std::process::ChildStdin>,
    mut stdout: pipe::Pipe<std::process::ChildStdout>,
    mut buffers: Buffers,
    stop: Arc<AtomicBool>,
    started: Instant,
    check: &(dyn Fn() -> Result<()> + Sync),
) -> (
    StagedNativeRows<'scene, 'output, 'session>,
    SessionSegmentLog<'session>,
    Result<()>,
) {
    let active = || {
        if stop.load(Ordering::SeqCst) {
            return Err("Streaming JXL operation stopped".into());
        }
        check()?;
        if started.elapsed() >= Duration::from_millis(u64::from(configuration.max_elapsed_ms)) {
            return Err("Streaming JXL operation timed out".into());
        }
        input.check(check)
    };
    let result = (|| -> Result<()> {
        let mut state = State::new(configuration, nonce)?;
        state.send(
            Direction::Parent,
            Message::Start(configuration),
            &mut stdin,
            &mut std::io::Cursor::new(input.profile()),
            &mut buffers.io,
            &active,
        )?;
        loop {
            active()?;
            let frame = Frame::read(&mut stdout, &active)?;
            let sequence = frame.sequence();
            let message = frame.message();
            state.preflight(Direction::Child, &frame)?;
            let mut copied = 0usize;
            state.complete(
                Direction::Child,
                frame,
                &mut stdout,
                &mut buffers.io,
                &active,
                &mut |bytes| {
                    let end = copied
                        .checked_add(bytes.len())
                        .filter(|n| *n <= buffers.segment.len())
                        .ok_or("Streaming segment body exceeds admission")?;
                    buffers.segment[copied..end].copy_from_slice(bytes);
                    copied = end;
                    Ok(())
                },
            )?;
            let event = match message {
                Message::Ready => SegmentEvent::Observed { wire_kind: 2 },
                Message::NeedRectangle(_) => SegmentEvent::Observed { wire_kind: 3 },
                Message::Release { .. } => SegmentEvent::Observed { wire_kind: 5 },
                Message::Seek { position } => SegmentEvent::Seek { position },
                Message::Finalized { position } => SegmentEvent::Finalized { position },
                Message::Segment {
                    offset,
                    bytes,
                    sha256,
                } => {
                    if copied != bytes as usize {
                        return Err("Streaming segment was incomplete".into());
                    }
                    SegmentEvent::Data { offset, sha256 }
                }
                Message::Done { bytes, records } => SegmentEvent::Done { bytes, records },
                _ => return Err("Unexpected streaming child event".into()),
            };
            let payload =
                matches!(message, Message::Segment { .. }).then_some((&buffers.segment, copied));
            log.append(sequence, event, payload, &mut || active())?;
            match message {
                Message::NeedRectangle(rectangle) => {
                    let (count, sha256) = fill_rectangle(&input, rectangle, &mut buffers, &active)?;
                    state.send(
                        Direction::Parent,
                        Message::RectangleData {
                            id: rectangle.id,
                            bytes: count as u64,
                            sha256,
                        },
                        &mut stdin,
                        &mut std::io::Cursor::new(&buffers.window[..count]),
                        &mut buffers.io,
                        &active,
                    )?;
                }
                Message::Done { .. } => {
                    if !state.successful() {
                        return Err("Streaming child lacks complete transport proof".into());
                    }
                    drop(stdin);
                    wire::exact_eof(&mut stdout, &active)?;
                    return active();
                }
                _ => state.send(
                    Direction::Parent,
                    Message::Continue { request: sequence },
                    &mut stdin,
                    &mut std::io::empty(),
                    &mut buffers.io,
                    &active,
                )?,
            }
        }
    })();
    (input, log, result)
}

type WorkerResult<'scene, 'output, 'session> = (
    Option<StagedNativeRows<'scene, 'output, 'session>>,
    Option<SessionSegmentLog<'session>>,
    Result<()>,
    bool,
);
#[allow(clippy::too_many_arguments)]
fn run_worker<'scene, 'output, 'session>(
    input: StagedNativeRows<'scene, 'output, 'session>,
    log: SessionSegmentLog<'session>,
    configuration: Configuration,
    nonce: [u8; 16],
    buffers: Buffers,
    resources: ChildResources,
    source: crate::workspace::source_runtime::SessionRetention,
    mut command: Command,
    started: Instant,
    check: &(dyn Fn() -> Result<()> + Sync),
) -> WorkerResult<'scene, 'output, 'session> {
    thread::scope(|scope| {
        let stop = Arc::new(AtomicBool::new(false));
        let child = match command.spawn() {
            Ok(child) => child,
            Err(_) => return (Some(input), Some(log), Err(FAILED.into()), true),
        };
        let mut guard = ChildGuard {
            child: Some(child),
            done: false,
            stop: stop.clone(),
            _limits: None,
            _source: Some(source),
            _resources: Some(resources),
            #[cfg(test)]
            primary_kill_failed: false,
            #[cfg(test)]
            kill_calls: 0,
        };
        let pipes = (|| -> Result<_> {
            guard._limits = Some(process_limits::Parent::new(
                guard.child.as_ref().ok_or(FAILED)?,
            )?);
            let stdin = pipe::Pipe::writer(
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
            let stdout = pipe::Pipe::reader(
                guard
                    .child
                    .as_mut()
                    .ok_or(FAILED)?
                    .stdout
                    .take()
                    .ok_or(FAILED)?,
                stop.clone(),
            )
            .map_err(|_| FAILED)?;
            Ok((stdin, stdout))
        })();
        let (stdin, stdout) = match pipes {
            Ok(value) => value,
            Err(error) => {
                let stopped = guard.stop();
                let known = guard.done;
                return (
                    Some(input),
                    Some(log),
                    Err(stopped.err().unwrap_or(error)),
                    known,
                );
            }
        };
        let (tx, rx) = mpsc::sync_channel(1);
        let pump = thread::Builder::new()
            .name("peerbrush-jxl-stream".into())
            .spawn_scoped(scope, move || {
                let packet = exchange(
                    input,
                    log,
                    configuration,
                    nonce,
                    stdin,
                    stdout,
                    buffers,
                    stop,
                    started,
                    check,
                );
                let _ = tx.send(packet);
            });
        let pump = match pump {
            Ok(pump) => pump,
            Err(_) => {
                let stopped = guard.stop();
                return (
                    None,
                    None,
                    Err(stopped.err().unwrap_or_else(|| {
                        "Streaming pump could not start; registered operation files retained".into()
                    })),
                    guard.done,
                );
            }
        };
        let mut packet = None;
        let operation = (|| -> Result<()> {
            loop {
                check()?;
                if started.elapsed()
                    >= Duration::from_millis(u64::from(configuration.max_elapsed_ms))
                {
                    return Err("Streaming JXL worker timed out".into());
                }
                match rx.try_recv() {
                    Ok(value) => {
                        let error = value.2.as_ref().err().cloned();
                        packet = Some(value);
                        if let Some(error) = error {
                            return Err(error);
                        }
                    }
                    Err(mpsc::TryRecvError::Empty) => {}
                    Err(mpsc::TryRecvError::Disconnected) if packet.is_some() => {}
                    Err(_) => return Err(FAILED.into()),
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
                    if packet.is_some() {
                        return Ok(());
                    }
                } else if let Err(error) =
                    process_limits::check(guard.child.as_ref().ok_or(FAILED)?)
                {
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
            }
        })();
        guard.stop.store(true, Ordering::SeqCst);
        let cleanup = if operation.is_err() {
            guard.stop()
        } else {
            Ok(())
        };
        let joined = pump.join().map_err(|_| {
            "Streaming pump panicked; registered operation files retained".to_string()
        });
        if packet.is_none() {
            packet = rx.try_recv().ok();
        }
        let known = guard.done;
        let Some((input, log, pumped)) = packet else {
            return (
                None,
                None,
                Err(cleanup
                    .err()
                    .or_else(|| joined.err())
                    .unwrap_or_else(|| "Streaming pump failed; operation files retained".into())),
                known,
            );
        };
        let outcome = cleanup.and(joined).and(operation).and(pumped);
        (Some(input), Some(log), outcome, known)
    })
}
/// Consumes only a closed original row stage. No arbitrary byte blob, worker
/// command, borrowed raw file path or externally supplied digest can enter.
pub(crate) fn encode<'scene, 'output, 'session>(
    input: StagedNativeRows<'scene, 'output, 'session>,
    limits: Limits,
    check: &(dyn Fn() -> Result<()> + Sync),
) -> Result<EncodedJxl<'scene, 'output, 'session>> {
    let session = input.session();
    let mut held = Some(input);
    let mut logging = None;
    let mut canonical = None;
    let mut may_retire = true;
    let result = (|| -> Result<(Configuration, [u8; 32])> {
        check()?;
        let input = held.as_ref().ok_or("Streaming input was consumed")?;
        let configuration = configuration(input, &limits)?;
        let started = Instant::now();
        ensure_reaper()?;
        if cleanup_failed() {
            return Err("JPEG XL cleanup is blocked; source work retained".into());
        }
        let _active = loop {
            check()?;
            match ACTIVE.try_lock() {
                Ok(value) => break value,
                Err(std::sync::TryLockError::Poisoned(_)) => return Err(FAILED.into()),
                Err(std::sync::TryLockError::WouldBlock) => {}
            }
            if started.elapsed() >= Duration::from_millis(u64::from(limits.deadline_ms)) {
                return Err("Streaming JXL queue timed out".into());
            }
            thread::sleep(Duration::from_millis(10));
        };
        if cleanup_failed() {
            return Err("JPEG XL cleanup is blocked; source work retained".into());
        }
        input.recheck(check)?;
        let nonce = *uuid::Uuid::new_v4().as_bytes();
        let binding = configuration.binding(nonce)?;
        logging = Some(session.segment_log(
            SegmentLimits {
                output_bytes: limits.output_bytes,
                protocol_log_bytes: limits.log_bytes,
                records: limits.records,
                events: wire::child_event_limit(&configuration)?,
            },
            nonce,
            binding,
        )?);
        let resources = resources(input)?;
        let buffers = Buffers::new(input)?;
        let source = session.retain_for_child()?;
        let executable = executable()?;
        let mut command = Command::new(&executable);
        command
            .current_dir(executable.parent().ok_or(FAILED)?)
            .arg("--jxl-stream-worker")
            .env_clear();
        #[cfg(windows)]
        for key in ["SystemRoot", "WINDIR"] {
            if let Some(value) = std::env::var_os(key) {
                command.env(key, value);
            }
        }
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            command.creation_flags(0x08000000);
        }
        let input = held.take().ok_or("Streaming input was consumed")?;
        let log = logging.take().ok_or("Streaming output log was consumed")?;
        let (input, log, outcome, known) = run_worker(
            input,
            log,
            configuration,
            nonce,
            buffers,
            resources,
            source,
            command,
            started,
            check,
        );
        held = input;
        logging = log;
        may_retire = known;
        outcome?;
        let input = held.as_ref().ok_or("Streaming input owner disappeared")?;
        input.recheck(check)?;
        let segments = logging
            .take()
            .ok_or("Streaming output log disappeared")?
            .finish(&mut || input.check(check))?;
        canonical = Some(segments.canonical(&mut || input.check(check))?);
        input.recheck(check)?;
        let output = canonical
            .as_ref()
            .ok_or("Streaming canonical output disappeared")?;
        output.check_session(session)?;
        let mut prefix = ScratchBytes::new(
            &session.resident(),
            output.bytes().payload_length().min(12) as usize,
            12,
        )
        .map_err(|e| e.to_string())?;
        let mut failure = None;
        output
            .bytes()
            .read_range_into(0, &mut prefix, &mut || {
                input.check(check).map_err(|e| {
                    failure = Some(e);
                    crate::large_document::Error::Cancelled
                })
            })
            .map_err(|e| failure.unwrap_or_else(|| e.to_string()))?;
        if !signature(&prefix) {
            return Err("Streaming output is not a JPEG XL signature".into());
        }
        check()?;
        Ok((configuration, binding))
    })();
    match result {
        Ok((configuration, binding)) => Ok(EncodedJxl {
            input: held.take().ok_or("Streaming input owner disappeared")?,
            output: canonical
                .take()
                .ok_or("Streaming output owner disappeared")?,
            configuration,
            binding,
            failed: AtomicBool::new(false),
        }),
        Err(error) => {
            if !may_retire {
                return Err(format!("{error}; source/profile/process owners and registered operation bytes retained"));
            }
            let mut refusal = None;
            if let Some(output) = canonical.take() {
                if let Err(e) = output.retire(&mut || session.cleanup_check()) {
                    refusal = Some(e);
                }
            }
            if let Some(log) = logging.take() {
                if let Err(e) = log.abort(&mut || session.cleanup_check()) {
                    refusal.get_or_insert(e);
                }
            }
            if let Some(input) = held.take() {
                if let Err(e) = input.retire(&mut || session.cleanup_check()) {
                    refusal.get_or_insert(e);
                }
            }
            Err(refusal.map_or(error.clone(), |cleanup| {
                format!("{error}; operation cleanup retained: {cleanup}")
            }))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[derive(Clone, Copy)]
    enum FixturePhase {
        Staging,
        Encode,
        Recheck,
        Copy,
        Decode,
        Pixels,
        Retire,
        Cleanup,
    }
    impl FixturePhase {
        fn label(self) -> &'static str {
            match self {
                Self::Staging => "staging",
                Self::Encode => "encode",
                Self::Recheck => "recheck",
                Self::Copy => "copy",
                Self::Decode => "decode",
                Self::Pixels => "pixels",
                Self::Retire => "retire",
                Self::Cleanup => "cleanup",
            }
        }
    }
    fn fixture_category(error: &str) -> &'static str {
        if error.contains("read work") || error.contains("budget") || error.contains("admission") {
            "quota"
        } else if error.contains("timed out") {
            "timeout"
        } else if error.contains("retained") {
            "retained"
        } else if error == FAILED {
            "worker"
        } else if error == PROTOCOL || error.contains("transport") {
            "protocol"
        } else {
            "other"
        }
    }
    fn fixture_mark(depth: u16, phase: FixturePhase, category: &'static str) {
        assert!(matches!(depth, 8 | 16));
        assert!(matches!(
            category,
            "begin" | "ok" | "quota" | "timeout" | "retained" | "worker" | "protocol" | "other"
        ));
        eprintln!(
            "JXL_STREAM_STATUS phase={} depth={} category={}",
            phase.label(),
            depth,
            category
        );
    }
    fn fixture_result<T>(depth: u16, phase: FixturePhase, result: Result<T>) -> T {
        match result {
            Ok(value) => {
                fixture_mark(depth, phase, "ok");
                value
            }
            Err(error) => {
                fixture_mark(depth, phase, fixture_category(&error));
                panic!("native streaming fixture refused at declared stage")
            }
        }
    }
    #[test]
    fn actual_profile_staging_owned_mode1_worker_replay_and_isolated_decode_keep_native_samples() {
        // Source-matched Cargo main is mandatory, just as for existing codec
        // integration. This executes no application UI or PNG decoder entry.
        let _serial = super::super::tests::PROCESS_FIXTURES
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        for depth in [8, 16] {
            fixture_mark(depth, FixturePhase::Staging, "begin");
            crate::production_export::staged_rows::tests::with_staged(depth, |input| {
                fixture_mark(depth, FixturePhase::Staging, "ok");
                fixture_mark(depth, FixturePhase::Encode, "begin");
                let session = input.session();
                let raw = input.raw_bytes();
                let profile = input.profile().to_vec();
                let output = fixture_result(
                    depth,
                    FixturePhase::Encode,
                    encode(
                        input,
                        Limits {
                            output_bytes: 1024 * 1024,
                            log_bytes: 2 * 1024 * 1024,
                            records: 4096,
                            input_bytes: wire::source_work_ceiling(raw).unwrap(),
                            deadline_ms: 15000,
                        },
                        &|| Ok(()),
                    ),
                );
                fixture_mark(depth, FixturePhase::Recheck, "begin");
                fixture_result(depth, FixturePhase::Recheck, output.recheck(&|| Ok(())));
                fixture_mark(depth, FixturePhase::Copy, "begin");
                let mut encoded =
                    ScratchBytes::new(&session.resident(), output.bytes() as usize, 1024 * 1024)
                        .unwrap();
                fixture_result(
                    depth,
                    FixturePhase::Copy,
                    output.copy_range(0, &mut encoded, &|| Ok(())),
                );
                assert_eq!(
                    ring::digest::digest(&SHA256, &encoded).as_ref(),
                    output.sha256()
                );
                fixture_mark(depth, FixturePhase::Decode, "begin");
                let executable = executable().unwrap();
                let mut command = Command::new(&executable);
                command
                    .arg("--jxl-worker")
                    .current_dir(executable.parent().unwrap());
                let decoded = fixture_result(
                    depth,
                    FixturePhase::Decode,
                    invoke(
                        request(Operation::Decode, None, None, 0, encoded.len()).unwrap(),
                        Input::Encoded(&encoded),
                        &|| Ok(()),
                        Runtime {
                            timeout: Duration::from_secs(15),
                            command: Some(command),
                            test_prefix: false,
                            response_started: None,
                            publication_started: None,
                            primary_kill_failed: false,
                            input_written: None,
                            stall_writer: false,
                        },
                    ),
                );
                fixture_mark(depth, FixturePhase::Pixels, "begin");
                assert_eq!(decoded.info.width, 17);
                assert_eq!(decoded.info.height, 513);
                assert_eq!(decoded.info.bit_depth, depth);
                assert_eq!(decoded.profile, profile);
                match decoded.pixels {
                    Pixels::Eight(bytes) => {
                        assert_eq!(depth, 8);
                        for (at, value) in bytes.iter().enumerate() {
                            assert_eq!(
                                *value,
                                crate::production_export::staged_rows::tests::expected_byte(
                                    at, depth
                                )
                            );
                        }
                    }
                    Pixels::Sixteen(words) => {
                        assert_eq!(depth, 16);
                        for (at, value) in words.iter().enumerate() {
                            assert_eq!(
                                value.to_be_bytes(),
                                [
                                    crate::production_export::staged_rows::tests::expected_byte(
                                        at * 2,
                                        depth
                                    ),
                                    crate::production_export::staged_rows::tests::expected_byte(
                                        at * 2 + 1,
                                        depth
                                    )
                                ]
                            );
                        }
                    }
                }
                fixture_mark(depth, FixturePhase::Pixels, "ok");
                assert!(output.source_read_bytes() <= wire::source_read_ceiling(raw).unwrap());
                drop(encoded);
                fixture_mark(depth, FixturePhase::Retire, "begin");
                fixture_result(
                    depth,
                    FixturePhase::Retire,
                    output.retire(&mut || session.cleanup_check()),
                );
                fixture_mark(depth, FixturePhase::Cleanup, "begin");
            });
            fixture_mark(depth, FixturePhase::Cleanup, "ok");
        }
    }
    #[test]
    fn malformed_real_child_stdout_cannot_complete_and_owned_stop_precedes_namespace_cleanup() {
        let _serial = super::super::tests::PROCESS_FIXTURES
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        crate::production_export::staged_rows::tests::with_staged(16, |input| {
            let session = input.session();
            let raw = input.raw_bytes();
            let limits = Limits {
                output_bytes: 1024 * 1024,
                log_bytes: 2 * 1024 * 1024,
                records: 128,
                input_bytes: wire::source_work_ceiling(raw).unwrap(),
                deadline_ms: 15000,
            };
            let config = configuration(&input, &limits).unwrap();
            let nonce = *uuid::Uuid::new_v4().as_bytes();
            let log = session
                .segment_log(
                    SegmentLimits {
                        output_bytes: limits.output_bytes,
                        protocol_log_bytes: limits.log_bytes,
                        records: limits.records,
                        events: wire::child_event_limit(&config).unwrap(),
                    },
                    nonce,
                    config.binding(nonce).unwrap(),
                )
                .unwrap();
            let resources = resources(&input).unwrap();
            let buffers = Buffers::new(&input).unwrap();
            let source = session.retain_for_child().unwrap();
            // Ordinary Cargo test listing is deliberately not our strict wire.
            // The real child must be stopped/reaped before cleanup is admitted.
            let mut command = Command::new(std::env::current_exe().unwrap());
            command
                .arg("--list")
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::null());
            #[cfg(windows)]
            {
                use std::os::windows::process::CommandExt;
                command.creation_flags(0x08000000);
            }
            let started = Instant::now();
            let (input, log, result, known) = run_worker(
                input,
                log,
                config,
                nonce,
                buffers,
                resources,
                source,
                command,
                started,
                &|| Ok(()),
            );
            assert!(result.is_err());
            assert!(
                known,
                "unknown child must retain its operation, never infer cleanup"
            );
            assert!(started.elapsed() < Duration::from_secs(18));
            log.unwrap().abort(&mut || session.cleanup_check()).unwrap();
            input
                .unwrap()
                .retire(&mut || session.cleanup_check())
                .unwrap();
        });
    }
    fn actual_legacy_runtime() -> Runtime {
        let executable = executable().unwrap();
        let mut command = Command::new(&executable);
        command
            .arg("--jxl-worker")
            .current_dir(executable.parent().unwrap());
        Runtime {
            timeout: Duration::from_secs(15),
            command: Some(command),
            test_prefix: false,
            response_started: None,
            publication_started: None,
            primary_kill_failed: false,
            input_written: None,
            stall_writer: false,
        }
    }
    #[test]
    fn lossy_rgb_with_lossless_alpha_keeps_native_output_contract_against_isolated_legacy_encoder()
    {
        let _serial = super::super::tests::PROCESS_FIXTURES
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        for depth in [8, 16] {
            let options = Options {
                lossless: false,
                distance: 1.,
                effort: 3,
            };
            crate::production_export::staged_rows::tests::with_staged_options(
                depth,
                33,
                2050,
                options,
                |input| {
                    let session = input.session();
                    let raw = input.raw_bytes();
                    let profile = input.profile().to_vec();
                    let mut samples =
                        ScratchBytes::new(&session.resident(), raw as usize, 1024 * 1024).unwrap();
                    input.copy_range(0, &mut samples, &|| Ok(())).unwrap();
                    let output = encode(
                        input,
                        Limits {
                            output_bytes: 1024 * 1024,
                            log_bytes: 2 * 1024 * 1024,
                            records: 4096,
                            input_bytes: wire::source_work_ceiling(raw).unwrap(),
                            deadline_ms: 15000,
                        },
                        &|| Ok(()),
                    )
                    .unwrap();
                    let mut encoded = ScratchBytes::new(
                        &session.resident(),
                        output.bytes() as usize,
                        1024 * 1024,
                    )
                    .unwrap();
                    output.copy_range(0, &mut encoded, &|| Ok(())).unwrap();
                    let decoded = invoke(
                        request(Operation::Decode, None, None, 0, encoded.len()).unwrap(),
                        Input::Encoded(&encoded),
                        &|| Ok(()),
                        actual_legacy_runtime(),
                    )
                    .unwrap();
                    let words: Vec<u16> = if depth == 16 {
                        samples
                            .chunks_exact(2)
                            .map(|v| u16::from_be_bytes([v[0], v[1]]))
                            .collect()
                    } else {
                        vec![]
                    };
                    let pixels = if depth == 8 {
                        PixelsRef::Eight(&samples)
                    } else {
                        PixelsRef::Sixteen(&words)
                    };
                    let reference = invoke(
                        request(
                            Operation::Encode,
                            Some(Info {
                                width: 33,
                                height: 2050,
                                bit_depth: depth,
                            }),
                            Some(options),
                            profile.len(),
                            raw as usize,
                        )
                        .unwrap(),
                        Input::Pixels(pixels, &profile),
                        &|| Ok(()),
                        actual_legacy_runtime(),
                    )
                    .unwrap();
                    let reference = invoke(
                        request(Operation::Decode, None, None, 0, reference.encoded.len()).unwrap(),
                        Input::Encoded(&reference.encoded),
                        &|| Ok(()),
                        actual_legacy_runtime(),
                    )
                    .unwrap();
                    assert_eq!(decoded.info, reference.info);
                    assert_eq!(decoded.info.bit_depth, depth);
                    // XYB decode can return a canonical sRGB ICC encoding. Compare
                    // actual unchanged decoder output, never relabel native samples.
                    assert_eq!(decoded.profile, reference.profile);
                    assert_eq!(decoded.pixels, reference.pixels);
                    match decoded.pixels {
                        Pixels::Eight(values) => {
                            for (a, b) in samples.chunks_exact(4).zip(values.chunks_exact(4)) {
                                assert_eq!(a[3], b[3]);
                            }
                        }
                        Pixels::Sixteen(values) => {
                            for (a, b) in samples.chunks_exact(8).zip(values.chunks_exact(4)) {
                                assert_eq!(&a[6..], b[3].to_be_bytes());
                            }
                        }
                    }
                    output.recheck(&|| Ok(())).unwrap();
                    drop(samples);
                    drop(encoded);
                    output.retire(&mut || session.cleanup_check()).unwrap();
                },
            );
        }
    }
}
