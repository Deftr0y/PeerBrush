use super::*;
use std::{
    io::Cursor,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
};
static PROCESS_FIXTURES: Mutex<()> = Mutex::new(());

// Rust's test harness emits a short banner before invoking this known entry.
// Only cfg(test) launchers may strip it. Real application protocol accepts
// MAGIC at byte zero and rejects every byte of native stdout noise.
pub(super) fn read_test_ready(input: &mut impl Read) -> Result<Response, String> {
    let mut window = [0; 8];
    for index in 0..1024 {
        window.rotate_left(1);
        read(input, &mut window[7..])?;
        if index >= 7 && &window == MAGIC {
            return read_header_body(input);
        }
    }
    Err(PROTOCOL.into())
}
#[cfg(any(target_os = "linux", target_os = "macos"))]
type NativeLimits = [(libc::rlim_t, libc::rlim_t); 3];

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn native_limits() -> Result<NativeLimits, String> {
    let read = |resource| {
        let mut value = std::mem::MaybeUninit::<libc::rlimit>::uninit();
        if unsafe { libc::getrlimit(resource, value.as_mut_ptr()) } != 0 {
            return Err(PROTOCOL.to_string());
        }
        let value = unsafe { value.assume_init() };
        Ok((value.rlim_cur, value.rlim_max))
    };
    Ok([
        read(libc::RLIMIT_CORE)?,
        read(libc::RLIMIT_CPU)?,
        read(libc::RLIMIT_AS)?,
    ])
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn verify_child_limits(inherited: NativeLimits) -> Result<(), String> {
    let address_space = if cfg!(target_os = "linux") {
        let budget = 1536 * 1024 * 1024;
        (inherited[2].0.min(budget), inherited[2].1.min(budget))
    } else {
        inherited[2]
    };
    let expected = [
        (0, 0),
        (inherited[1].0.min(120), inherited[1].1.min(120)),
        address_space,
    ];
    if native_limits()? != expected {
        return Err("JPEG XL fixture child resource policy mismatch".into());
    }
    Ok(())
}

fn timeout_category(error: &str) -> &'static str {
    match error {
        FAILED => "FAILED",
        PROTOCOL => "PROTOCOL",
        "JPEG XL worker resource protection is unavailable or exceeded" => "RESOURCE",
        _ => "OTHER",
    }
}

#[test]
fn timeout_diagnostic_exposes_only_fixed_worker_categories() {
    for (error, expected) in [
        (FAILED, "FAILED"),
        (PROTOCOL, "PROTOCOL"),
        (
            "JPEG XL worker resource protection is unavailable or exceeded",
            "RESOURCE",
        ),
        ("PRIVATE_WORKER_DETAIL_OR_PATH", "OTHER"),
    ] {
        assert_eq!(timeout_category(error), expected);
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn completed_native_workers_apply_child_limits_and_preserve_parent_policy() {
    let _serial = PROCESS_FIXTURES.lock().unwrap();
    let inherited = native_limits().unwrap();
    for depth in [8, 16] {
        // The exact known child verifies actual OS limits after successful native work.
        healthy_child(depth);
    }
    assert_eq!(native_limits().unwrap(), inherited);
}

fn fault() -> String {
    std::env::var("PEERBRUSH_JXL_TEST_FAULT").unwrap_or_default()
}
pub(super) fn ready_response(mut ready: Response) -> Response {
    match fault().as_str() {
        "wrong_nonce" => ready.nonce.replace_range(
            ..1,
            if ready.nonce.starts_with('0') {
                "1"
            } else {
                "0"
            },
        ),
        "wrong_build" => ready.build = "Unmatched synthetic build".into(),
        _ => {}
    }
    ready
}
pub(super) fn before_body(request: &Request, output: &mut impl Write) -> Result<bool, String> {
    if fault() == "stall_input" {
        thread::sleep(Duration::from_secs(60));
    }
    if fault() == "early_success" {
        let mut response = request.response(Kind::Result);
        response.info = Some(Info {
            width: 3,
            height: 2,
            bit_depth: 8,
        });
        header(output, &response)?;
        return Ok(true);
    }
    Ok(false)
}
pub(super) fn test_fault(request: &Request, output: &mut impl Write) -> Result<bool, String> {
    let selected = fault();
    match selected.as_str() {
        "exit" => std::process::exit(17),
        "abort" => std::process::abort(),
        "sleep" => {
            thread::sleep(Duration::from_secs(60));
            return Ok(true);
        }
        "noise" => {
            write(output, b"Unexpected native stdout")?;
            return Ok(true);
        }
        "truncated" | "oversized" | "partial" => {}
        _ => return Ok(false),
    }
    let depth = std::env::var("PEERBRUSH_JXL_TEST_DEPTH")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(8);
    let info = Info {
        width: 3,
        height: 2,
        bit_depth: depth,
    };
    let profile = crate::color_profile::srgb_profile();
    let mut response = request.response(Kind::Result);
    response.info = Some(info);
    response.profile_bytes = profile.len() as u32;
    response.payload_bytes = if selected == "oversized" {
        u64::MAX
    } else {
        raw_size(info)? as u64
    };
    header(output, &response)?;
    if selected == "oversized" {
        return Ok(true);
    }
    write(output, profile)?;
    write(output, &[0, 0])?;
    output.flush().map_err(|_| FAILED)?;
    if selected == "partial" {
        thread::sleep(Duration::from_secs(60));
    }
    Ok(true)
}

#[test]
#[ignore = "Internal isolated test worker entry; never an acceptance test by itself"]
fn worker_entry() {
    if std::env::var("PEERBRUSH_JXL_TEST_ENTRY").ok().as_deref() != Some("1") {
        return;
    }
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    let inherited = match native_limits() {
        Ok(value) => value,
        Err(_) => std::process::exit(3),
    };
    let result = worker();
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    let result = result.and_then(|()| verify_child_limits(inherited));
    std::process::exit(if result.is_ok() { 0 } else { 2 });
}
pub(super) fn runtime(
    fault: &str,
    depth: u16,
    timeout: Duration,
    started: Option<Arc<AtomicBool>>,
) -> Runtime {
    let mut command = Command::new(std::env::current_exe().unwrap());
    command.args([
        "--exact",
        "jpeg_xl::tests::worker_entry",
        "--ignored",
        "--nocapture",
        "--test-threads=1",
        "--color=never",
    ]);
    command.env_clear();
    #[cfg(windows)]
    for key in ["SystemRoot", "WINDIR"] {
        if let Some(value) = std::env::var_os(key) {
            command.env(key, value);
        }
    }
    command
        .env("PEERBRUSH_JXL_TEST_ENTRY", "1")
        .env("PEERBRUSH_JXL_TEST_FAULT", fault)
        .env("PEERBRUSH_JXL_TEST_DEPTH", depth.to_string());
    Runtime {
        timeout,
        command: Some(command),
        test_prefix: true,
        response_started: started,
        primary_kill_failed: false,
        input_written: None,
        stall_writer: false,
    }
}

fn healthy_child(depth: u16) {
    let rgba = vec![12347_u16; 3 * 2 * 4];
    let bytes = vec![47_u8; 3 * 2 * 4];
    let pixels = if depth == 8 {
        PixelsRef::Eight(&bytes)
    } else {
        PixelsRef::Sixteen(&rgba)
    };
    let profile = crate::color_profile::srgb_profile();
    let info = Info {
        width: 3,
        height: 2,
        bit_depth: depth,
    };
    let request = super::request(
        Operation::Encode,
        Some(info),
        Some(Options::default()),
        profile.len(),
        raw_size(info).unwrap(),
    )
    .unwrap();
    let result = invoke(
        request,
        Input::Pixels(pixels, profile),
        &|| Ok(()),
        runtime("", depth, Duration::from_secs(10), None),
    )
    .unwrap();
    assert!(signature(&result.encoded));
}

#[test]
fn stalled_unconsumed_large_input_cancels_or_times_out_and_kill_fallback_recovers() {
    let _serial = PROCESS_FIXTURES.lock().unwrap();
    use std::sync::atomic::AtomicUsize;
    for depth in [8, 16] {
        let doc = document(depth);
        let before = serde_json::to_value(&doc).unwrap();
        let native = doc.layers[0].pixels.rgba16();
        // The child acknowledges the exact header then never reads this body.
        // Eight MiB exceeds the anonymous pipe capacity on all supported OSes.
        let mut bytes = vec![0; 8 * 1024 * 1024];
        bytes[..2].copy_from_slice(&[0xff, 0x0a]);
        for timeout in [false, true] {
            let written = Arc::new(AtomicUsize::new(0));
            let started = Instant::now();
            let check = || {
                if !timeout
                    && written.load(Ordering::SeqCst) > 0
                    && started.elapsed() > Duration::from_millis(200)
                {
                    Err("fixture input canceled".into())
                } else {
                    Ok(())
                }
            };
            let mut runtime = runtime(
                "stall_input",
                depth,
                Duration::from_secs(if timeout { 3 } else { 10 }),
                None,
            );
            runtime.input_written = Some(written.clone());
            // Simulate failure of the first Child::kill call; the Windows
            // job or owned Unix retry must still stop/reap the actual child.
            runtime.primary_kill_failed = true;
            let request = super::request(Operation::Decode, None, None, 0, bytes.len()).unwrap();
            let error = invoke(request, Input::Encoded(&bytes), &check, runtime)
                .err()
                .unwrap();
            assert!(written.load(Ordering::SeqCst) > 0);
            assert!(written.load(Ordering::SeqCst) < bytes.len());
            assert!(if timeout {
                error.contains("timed out")
            } else {
                error == "fixture input canceled"
            });
            assert!(!FAILED_STOP.load(Ordering::SeqCst));
            assert!(RETAINED.lock().unwrap().is_none());
            assert_eq!(serde_json::to_value(&doc).unwrap(), before);
            assert_eq!(doc.layers[0].pixels.rgba16(), native);
            healthy_child(depth);
        }
    }
}

#[test]
fn known_reaped_child_never_reaches_primary_or_fallback_kill() {
    let _serial = PROCESS_FIXTURES.lock().unwrap();
    let mut command = runtime("", 8, DEADLINE, None).command.take().unwrap();
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000);
    }
    // EOF without a request exits the known worker before any native work.
    let mut child = command.spawn().unwrap();
    assert!(!child.wait().unwrap().success());
    let stop = Arc::new(AtomicBool::new(false));
    let mut guard = ChildGuard {
        child: Some(child),
        done: false,
        stop: stop.clone(),
        _limits: None,
        primary_kill_failed: true,
        kill_calls: 0,
    };
    guard.stop().unwrap();
    assert!(guard.done);
    assert!(stop.load(Ordering::SeqCst));
    assert_eq!(guard.kill_calls, 0);
    assert!(!FAILED_STOP.load(Ordering::SeqCst));
    assert!(RETAINED.lock().unwrap().is_none());
}

#[test]
fn early_success_also_stops_an_unfinished_writer_before_joining() {
    let _serial = PROCESS_FIXTURES.lock().unwrap();
    // A deterministic writer hook models a surviving inherited pipe reader
    // without forking an orphan. Actual blocked-pipe I/O is covered above.
    let mut runtime = runtime("early_success", 8, Duration::from_secs(10), None);
    runtime.stall_writer = true;
    let request = super::request(Operation::Inspect, None, None, 0, 2).unwrap();
    assert!(invoke(request, Input::Encoded(&[0xff, 0x0a]), &|| Ok(()), runtime).is_err());
    healthy_child(8);
}
fn document(depth: u16) -> crate::engine::Document {
    let mut doc = crate::engine::Document::new_depth(3, 2, depth).unwrap();
    if depth == 16 {
        doc.layers[0]
            .pixels
            .set16(1, 1, [12347, 23459, 34571, 45679]);
    } else {
        doc.layers[0].pixels.set(1, 1, [47, 95, 143, 191]);
    }
    doc
}
#[test]
fn strict_metadata_lengths_nonce_source_and_trailing_bytes_fail_before_output() {
    let request = super::request(Operation::Decode, None, None, 0, 2).unwrap();
    request.validate().unwrap();
    for damage in [0, 1, 2, 3] {
        let mut invalid = request.clone();
        match damage {
            0 => invalid.payload_bytes = u64::MAX,
            1 => invalid.build = "wrong".into(),
            2 => invalid.profile_bytes = 1,
            _ => invalid.nonce = "not a nonce".into(),
        }
        assert!(invalid.validate().is_err());
    }
    let mut oversized = Vec::new();
    oversized.extend(MAGIC);
    oversized.extend(u32::MAX.to_le_bytes());
    assert!(read_header::<Request>(&mut Cursor::new(oversized)).is_err());
    let mut packet = Vec::new();
    header(&mut packet, &request).unwrap();
    packet.pop();
    assert!(read_header::<Request>(&mut Cursor::new(packet)).is_err());
    let mut result = request.response(Kind::Result);
    result.info = Some(Info {
        width: 3,
        height: 2,
        bit_depth: 16,
    });
    result.profile_bytes = 1;
    result.payload_bytes = u64::MAX;
    assert!(result.validate(&request).is_err());
    result.payload_bytes = 48;
    result.profile_bytes = crate::color_profile::MAX_PROFILE_BYTES as u32 + 1;
    assert!(result.validate(&request).is_err());
    assert!(eof(&mut Cursor::new([1])).is_err());
}
#[test]
fn abnormal_exit_abort_protocol_damage_and_timeout_preserve_parent_native_work() {
    let _serial = PROCESS_FIXTURES.lock().unwrap();
    for depth in [8, 16] {
        let doc = document(depth);
        let before = serde_json::to_value(&doc).unwrap();
        let native = doc.layers[0].pixels.rgba16();
        for selected in [
            "exit",
            "abort",
            "noise",
            "truncated",
            "oversized",
            "wrong_nonce",
            "wrong_build",
        ] {
            let request = super::request(Operation::Decode, None, None, 0, 2).unwrap();
            let result = invoke(
                request,
                Input::Encoded(&[0xff, 0x0a]),
                &|| Ok(()),
                runtime(selected, depth, Duration::from_secs(10), None),
            );
            assert!(result.is_err(), "Isolated fault was accepted: {selected}");
            assert_eq!(serde_json::to_value(&doc).unwrap(), before);
            assert_eq!(doc.layers[0].pixels.rgba16(), native);
        }
        let request = super::request(Operation::Decode, None, None, 0, 2).unwrap();
        let error = invoke(
            request,
            Input::Encoded(&[0xff, 0x0a]),
            &|| Ok(()),
            runtime("sleep", depth, Duration::from_secs(3), None),
        )
        .err()
        .unwrap();
        assert!(
            error.contains("timed out"),
            "JXL_TIMEOUT_ERROR_{}",
            timeout_category(&error)
        );
        assert_eq!(serde_json::to_value(&doc).unwrap(), before);
    }
}
#[test]
fn cancellation_during_partial_native_depth_output_kills_reaps_and_discards_it() {
    let _serial = PROCESS_FIXTURES.lock().unwrap();
    for depth in [8, 16] {
        let doc = document(depth);
        let before = serde_json::to_value(&doc).unwrap();
        let native = doc.layers[0].pixels.rgba16();
        let started = Arc::new(AtomicBool::new(false));
        let check = || {
            if started.load(Ordering::SeqCst) {
                Err("fixture canceled".into())
            } else {
                Ok(())
            }
        };
        let request = super::request(Operation::Decode, None, None, 0, 2).unwrap();
        let error = invoke(
            request,
            Input::Encoded(&[0xff, 0x0a]),
            &check,
            runtime(
                "partial",
                depth,
                Duration::from_secs(10),
                Some(started.clone()),
            ),
        )
        .err()
        .unwrap();
        assert_eq!(error, "fixture canceled");
        assert!(started.load(Ordering::SeqCst));
        assert_eq!(serde_json::to_value(&doc).unwrap(), before);
        assert_eq!(doc.layers[0].pixels.rgba16(), native);
        // The failed child must release its single active slot; a new trusted
        // child can encode normally after cancellation, with no global mutation.
        healthy_child(depth);
        assert_eq!(serde_json::to_value(&doc).unwrap(), before);
    }
}
