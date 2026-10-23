use super::*;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};

struct Temp(PathBuf);
impl Temp {
    fn new() -> (Self, File) {
        let path =
            std::env::temp_dir().join(format!("peerbrush-capture-ipc-{}", uuid::Uuid::new_v4()));
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)
            .unwrap();
        file.set_len(19).unwrap();
        (Self(path), file)
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}
fn request() -> Request {
    Request {
        schema: 1,
        nonce: "3".repeat(64),
        parent_pid: std::process::id(),
        checkpoint: uuid::Uuid::new_v4().to_string(),
        manifest_sha256: "a".repeat(64),
        build: crate::build_info::json().into(),
    }
}
#[test]
fn ordinary_arguments_never_enter_capture_mode_or_change_existing_dispatch() {
    for arguments in [
        vec![],
        vec!["--version"],
        vec!["--build-info"],
        vec!["--help"],
        vec!["mcp"],
        vec!["updates", "check"],
        vec!["settings", "paths"],
        vec!["--headless"],
        vec!["--state-dir", "private", "art.psd"],
        vec!["--open", "art.psb"],
    ] {
        let arguments: Vec<_> = arguments
            .into_iter()
            .map(std::ffi::OsString::from)
            .collect();
        assert_eq!(
            dispatch(&arguments, || panic!("ordinary launch entered capture")),
            None
        );
    }
}
#[test]
fn reserved_entry_refuses_extra_reordered_and_payload_arguments_before_worker() {
    for arguments in [
        vec![ENTRY, "target.app"],
        vec!["--state-dir", "private", ENTRY],
        vec![ENTRY, "--nonce", "secret"],
        vec![ENTRY, ENTRY],
    ] {
        let arguments: Vec<_> = arguments
            .into_iter()
            .map(std::ffi::OsString::from)
            .collect();
        assert_eq!(
            dispatch(&arguments, || panic!("malformed launch entered worker")),
            Some(2)
        );
    }
    let called = std::cell::Cell::new(false);
    assert_eq!(
        dispatch(&[ENTRY.into()], || {
            called.set(true);
            Err(FAILED.into())
        }),
        Some(2)
    );
    assert!(called.get());
}
#[test]
fn actual_inherited_request_guard_refuses_foreign_stale_and_malformed_claims_before_native_authority(
) {
    for damage in [
        "none",
        "schema",
        "nonce",
        "parent",
        "checkpoint",
        "hash",
        "source",
        "unknown",
    ] {
        let (_temp, file) = Temp::new();
        let (mut parent, mut child) = UnixStream::pair().unwrap();
        let cancel = AtomicBool::new(false);
        let deadline = Instant::now() + Duration::from_secs(2);
        let mut request = serde_json::to_value(request()).unwrap();
        match damage {
            "schema" => request["schema"] = serde_json::json!(2),
            "nonce" => request["nonce"] = serde_json::json!("forged"),
            "parent" => request["parent_pid"] = serde_json::json!(1),
            "checkpoint" => request["checkpoint"] = serde_json::json!("../foreign"),
            "hash" => request["manifest_sha256"] = serde_json::json!("f".repeat(63)),
            "source" => request["build"] = serde_json::json!("stale source"),
            "unknown" => request["target"] = serde_json::json!("arbitrary.app"),
            _ => {}
        }
        send(&mut parent, &request, deadline, &cancel).unwrap();
        let authorized = std::cell::Cell::new(false);
        let result = accept(&mut child, std::process::id(), deadline, &cancel, |_, _| {
            authorized.set(true);
            Err("native refusal fixture".into())
        });
        assert!(result.is_err());
        assert_eq!(authorized.get(), damage == "none", "{damage}");
        assert_eq!(file.metadata().unwrap().len(), 19);
    }
}
#[test]
fn inherited_descriptor_is_cloexec_and_closing_it_does_not_change_parent_bytes() {
    let (_temp, file) = Temp::new();
    let before = file.metadata().unwrap();
    let (parent, child) = UnixStream::pair().unwrap();
    send_lock(&parent, &file).unwrap();
    let received = receive_lock(
        &child,
        Instant::now() + Duration::from_secs(2),
        &AtomicBool::new(false),
    )
    .unwrap();
    let after = received.metadata().unwrap();
    assert_eq!(
        (before.dev(), before.ino(), before.len()),
        (after.dev(), after.ino(), after.len())
    );
    assert_ne!(
        unsafe { libc::fcntl(received.as_raw_fd(), libc::F_GETFD) } & libc::FD_CLOEXEC,
        0
    );
    drop(received);
    assert_eq!(file.metadata().unwrap().len(), 19);
}
#[test]
fn actual_framed_receiver_refuses_limits_truncation_timeout_and_cancel() {
    for damage in ["large", "truncated", "cancel", "deadline"] {
        let (mut parent, mut child) = UnixStream::pair().unwrap();
        match damage {
            "large" => parent
                .write_all(&((FRAME_LIMIT + 1) as u32).to_be_bytes())
                .unwrap(),
            "truncated" => {
                parent.write_all(&10_u32.to_be_bytes()).unwrap();
                parent.write_all(b"{}").unwrap();
                parent.shutdown(std::net::Shutdown::Write).unwrap();
            }
            _ => {}
        }
        let deadline = if damage == "deadline" {
            Instant::now() - Duration::from_secs(1)
        } else {
            Instant::now() + Duration::from_secs(2)
        };
        let cancel = AtomicBool::new(damage == "cancel");
        assert!(
            receive::<Request>(&mut child, deadline, &cancel).is_err(),
            "{damage}"
        );
    }
}
#[test]
fn captured_reply_is_bound_to_exact_request_and_child_but_never_install_authority() {
    let request = request();
    for damage in [
        "none",
        "nonce",
        "pid",
        "checkpoint",
        "manifest",
        "source",
        "schema",
        "phase",
    ] {
        let mut response = Captured {
            kind: ReplyKind::Captured,
            schema: 1,
            nonce: request.nonce.clone(),
            owner_pid: 17,
            checkpoint: request.checkpoint.clone(),
            manifest_sha256: request.manifest_sha256.clone(),
            build: request.build.clone(),
        };
        match damage {
            "nonce" => response.nonce = "4".repeat(64),
            "pid" => response.owner_pid = 18,
            "checkpoint" => response.checkpoint = uuid::Uuid::new_v4().to_string(),
            "manifest" => response.manifest_sha256 = "b".repeat(64),
            "source" => response.build = "stale".into(),
            "schema" => response.schema = 2,
            "phase" => response.kind = ReplyKind::Authenticated,
            _ => {}
        }
        assert_eq!(
            response.validate(&request, 17, ReplyKind::Captured).is_ok(),
            damage == "none",
            "{damage}"
        );
        assert!(checkpoint::require_captured_handoff().is_err());
    }
}
#[test]
fn dropping_capture_channel_only_disconnects_our_owner_without_native_outcome() {
    let (parent, mut child) = UnixStream::pair().unwrap();
    let owner = Owner {
        socket: parent,
        child: None,
    };
    drop(owner);
    let mut byte = [0_u8];
    assert_eq!(child.read(&mut byte).unwrap(), 0);
    assert!(checkpoint::require_captured_handoff().is_err());
}

#[test]
fn actual_descriptor_receiver_refuses_missing_multiple_truncated_and_wrong_marker_messages() {
    for damage in ["missing", "multiple", "truncated", "marker"] {
        let (_temp, file) = Temp::new();
        let (mut parent, child) = UnixStream::pair().unwrap();
        if damage == "missing" || damage == "marker" {
            parent
                .write_all(&[if damage == "marker" { 0x01 } else { 0x50 }])
                .unwrap();
        } else {
            let count = if damage == "multiple" { 2 } else { 16 };
            let mut marker = [0x50_u8];
            let mut iov = libc::iovec {
                iov_base: marker.as_mut_ptr().cast(),
                iov_len: 1,
            };
            let mut control = [0_usize; 16];
            let mut message: libc::msghdr = unsafe { std::mem::zeroed() };
            message.msg_iov = &mut iov;
            message.msg_iovlen = 1;
            message.msg_control = control.as_mut_ptr().cast();
            message.msg_controllen =
                unsafe { libc::CMSG_SPACE(count * std::mem::size_of::<i32>() as u32) };
            unsafe {
                let header = libc::CMSG_FIRSTHDR(&message);
                (*header).cmsg_level = libc::SOL_SOCKET;
                (*header).cmsg_type = libc::SCM_RIGHTS;
                (*header).cmsg_len = libc::CMSG_LEN(count * std::mem::size_of::<i32>() as u32);
                for i in 0..count {
                    libc::CMSG_DATA(header)
                        .cast::<i32>()
                        .add(i as usize)
                        .write_unaligned(file.as_raw_fd());
                }
                assert_eq!(libc::sendmsg(parent.as_raw_fd(), &message, 0), 1);
            }
        }
        assert!(
            receive_lock(
                &child,
                Instant::now() + Duration::from_secs(2),
                &AtomicBool::new(false)
            )
            .is_err(),
            "{damage}"
        );
        assert_eq!(file.metadata().unwrap().len(), 19);
    }
}

#[test]
fn reaper_establishment_failure_never_enters_actual_process_spawn_boundary() {
    let (parent, mut peer) = UnixStream::pair().unwrap();
    let spawned = std::cell::Cell::new(false);
    let result = spawn_owned(
        parent,
        || Err("reaper establishment fixture".into()),
        || {
            spawned.set(true);
            panic!("process creation must not follow failed reaper establishment")
        },
    );
    assert!(result.is_err());
    assert!(!spawned.get());
    let mut byte = [0_u8];
    assert_eq!(peer.read(&mut byte).unwrap(), 0);
    assert!(checkpoint::require_captured_handoff().is_err());
}

#[test]
fn every_post_spawn_error_or_drop_waits_exact_child_once_on_reserved_worker() {
    for failure in [
        "channel setup",
        "handshake",
        "ownership transfer",
        "normal drop",
    ] {
        let (parent, peer) = UnixStream::pair().unwrap();
        let child_fd: OwnedFd = peer.into();
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .args([
                "--exact",
                "sparkle::macos::coordinator::tests::capture_reaper_child_entry",
                "--ignored",
                "--nocapture",
                "--test-threads=1",
                "--color=never",
            ])
            .env_clear()
            .env("PEERBRUSH_CAPTURE_REAPER_CHILD", "1")
            .stdin(Stdio::from(child_fd))
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let (waited, result) = std::sync::mpsc::channel();
        let caller = std::thread::current().id();
        let child_pid = std::cell::Cell::new(0);
        let operation = (|| -> Result<Owner, String> {
            let owner = spawn_owned(
                parent,
                || {
                    Reaper::with_wait(move |mut child| {
                        let pid = child.id();
                        let code = child.wait().map(|status| status.code());
                        let _ = waited.send((pid, std::thread::current().id(), code));
                    })
                },
                || command.spawn().map_err(|_| FAILED.to_string()),
            )?;
            child_pid.set(owner.child.as_ref().unwrap().0.id());
            // These are explicit setup/native-handshake/transfer error boundaries;
            // no publisher/feed/native updater is faked into success.
            if failure != "normal drop" {
                return Err(failure.into());
            }
            Ok(owner)
        })();
        match operation {
            Ok(owner) => {
                assert_eq!(failure, "normal drop");
                drop(owner);
            }
            Err(error) => assert_eq!(error, failure),
        }
        assert!(child_pid.get() > 1);
        let (actual, worker, code) = result.recv_timeout(Duration::from_secs(30)).unwrap();
        assert_eq!(actual, child_pid.get(), "{failure}");
        assert_ne!(worker, caller, "wait must never run on the owner/UI caller");
        assert_eq!(code.unwrap(), Some(37));
        assert!(matches!(
            result.recv_timeout(Duration::from_secs(30)),
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected)
        ));
        assert!(checkpoint::require_captured_handoff().is_err());
    }
}

#[test]
#[ignore = "Internal isolated reaper child; exercised only by the parent lifetime test"]
fn capture_reaper_child_entry() {
    if std::env::var("PEERBRUSH_CAPTURE_REAPER_CHILD")
        .ok()
        .as_deref()
        != Some("1")
    {
        return;
    }
    let mut byte = [0_u8];
    // Parent error/drop shuts down only its owned socket. Exit code37 binds the
    // observed wait to this real exact child rather than a nonexistent test.
    std::process::exit(if std::io::stdin().read(&mut byte).ok() == Some(0) {
        37
    } else {
        38
    });
}
