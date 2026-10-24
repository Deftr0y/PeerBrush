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
        request: None,
    };
    drop(owner);
    let mut byte = [0_u8];
    assert_eq!(child.read(&mut byte).unwrap(), 0);
    assert!(checkpoint::require_captured_handoff().is_err());
}

#[test]
fn owner_check_frames_reject_replay_foreign_binding_unknown_actions_and_fake_native_authority() {
    for damage in [
        "none",
        "nonce",
        "checkpoint",
        "manifest",
        "sequence",
        "zero",
        "schema",
        "unknown",
        "install",
    ] {
        let request = request();
        let (mut parent, mut child) = UnixStream::pair().unwrap();
        let cancel = AtomicBool::new(false);
        let deadline = Instant::now() + Duration::from_secs(2);
        let mut command =
            serde_json::to_value(CheckCommand::new(&request, 1, CheckAction::Check)).unwrap();
        match damage {
            "nonce" => command["nonce"] = serde_json::json!("a".repeat(64)),
            "checkpoint" => {
                command["checkpoint"] = serde_json::json!(uuid::Uuid::new_v4().to_string())
            }
            "manifest" => command["manifest_sha256"] = serde_json::json!("b".repeat(64)),
            "sequence" => command["sequence"] = serde_json::json!(2),
            "zero" => command["sequence"] = serde_json::json!(0),
            "schema" => command["schema"] = serde_json::json!(2),
            "unknown" => command["continuation"] = serde_json::json!(1),
            "install" => command["action"] = serde_json::json!("install"),
            _ => {}
        }
        send(&mut parent, &command, deadline, &cancel).unwrap();
        let native = std::cell::Cell::new(false);
        assert!(
            accept_check_command(&mut child, &request, 1, deadline, &cancel, |_| {
                native.set(true);
                Err("native authentication fixture refuses".into())
            })
            .is_err()
        );
        assert_eq!(native.get(), damage == "none", "{damage}");
    }
    let request = request();
    for action in [CheckAction::Check, CheckAction::Status, CheckAction::Cancel] {
        let command = CheckCommand::new(&request, 9, action);
        assert_eq!(
            command.validate(&request, 9).is_ok(),
            action != CheckAction::Check
        );
        assert!(command.validate(&request, 8).is_err());
    }
}

fn observation() -> NativeStatus {
    NativeStatus {
        phase: crate::sparkle::check::Phase::Quarantined,
        version: Some("0.30.1".into()),
        busy: false,
        cancelled: false,
        quarantine: true,
        native_cycle: true,
        held_generation: Some(7),
        found_generation: None,
        download_generation: None,
        expected_bytes: None,
        received_bytes: 0,
        extraction: None,
        download_complete: false,
        final_profile: None,
    }
}
#[test]
fn native_observations_bind_exact_child_sequence_action_and_never_clear_unknown_holds() {
    let request = request();
    for damage in [
        "none",
        "pid",
        "sequence",
        "action",
        "version",
        "zero",
        "cycle",
        "quarantine",
    ] {
        let mut reply = CheckReply {
            binding: CheckCommand::new(&request, 3, CheckAction::Status),
            owner_pid: 17,
            status: observation(),
            final_control: None,
        };
        match damage {
            "pid" => reply.owner_pid = 18,
            "sequence" => reply.binding.sequence = 2,
            "action" => reply.binding.action = CheckAction::Cancel,
            "version" => reply.status.version = Some("bad\nversion".into()),
            "zero" => reply.status.held_generation = Some(0),
            "cycle" => reply.status.native_cycle = false,
            "quarantine" => reply.status.quarantine = false,
            _ => {}
        }
        assert_eq!(
            reply
                .validate(&request, 17, 3, CheckAction::Status, None, None)
                .is_ok(),
            damage == "none",
            "{damage}"
        );
    }
    let state = driver::State::default();
    state.session.borrow_mut().begin().unwrap();
    state.session.borrow_mut().checking();
    state.native_starting().unwrap();
    let mut absent_claim = observation();
    absent_claim.native_cycle = false;
    absent_claim.held_generation = None;
    absent_claim.quarantine = false;
    absent_claim.phase = crate::sparkle::check::Phase::NoUpdate;
    state.observe_owner(absent_claim);
    assert!(state.view().native_cycle && state.view().quarantine);
    state.cancel();
    state.finished();
    assert!(state.view().native_cycle && state.view().quarantine);
    assert!(state.session.borrow_mut().begin().is_err());
}

#[test]
fn copied_check_parent_transport_refuses_before_reader_consumption_or_native_authority() {
    let (parent, mut peer) = UnixStream::pair().unwrap();
    let request = request();
    let nonce = request.nonce.clone();
    let mut owner = Owner {
        socket: parent,
        child: None,
        request: Some(request),
    };
    let error = owner
        .check("ABCD123456".into(), &eframe::egui::Context::default())
        .err()
        .unwrap();
    assert_eq!(error, checkpoint::require_captured_handoff().unwrap_err());
    assert_eq!(
        owner.request.as_ref().unwrap().nonce,
        nonce,
        "Refusal precedes consuming the sole reader grant"
    );
    peer.set_nonblocking(true).unwrap();
    assert_eq!(
        peer.read(&mut [0_u8]).unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock
    );
    assert!(checkpoint::require_captured_handoff().is_err());
}

#[test]
fn copied_check_transport_decodes_one_authenticated_frame_and_rejects_partial_or_unbounded_frames()
{
    let request = request();
    let (mut parent, mut child) = UnixStream::pair().unwrap();
    let cancel = AtomicBool::new(false);
    let deadline = Instant::now() + Duration::from_secs(2);
    send(
        &mut parent,
        &CheckCommand::new(&request, 1, CheckAction::Cancel),
        deadline,
        &cancel,
    )
    .unwrap();
    let received =
        accept_check_command(&mut child, &request, 1, deadline, &cancel, |_| Ok(())).unwrap();
    assert_eq!(
        received.action,
        CheckAction::Cancel,
        "The injected authentication boundary supplies no native ownership"
    );
    for damage in ["oversize", "partial", "cancel", "deadline"] {
        let (mut parent, mut child) = UnixStream::pair().unwrap();
        match damage {
            "oversize" => parent
                .write_all(&((FRAME_LIMIT + 1) as u32).to_be_bytes())
                .unwrap(),
            "partial" => {
                parent.write_all(&9_u32.to_be_bytes()).unwrap();
                parent.write_all(b"{}").unwrap();
                parent.shutdown(std::net::Shutdown::Write).unwrap();
            }
            _ => {}
        }
        let cancel = AtomicBool::new(damage == "cancel");
        let deadline = if damage == "deadline" {
            Instant::now() - Duration::from_secs(1)
        } else {
            deadline
        };
        assert!(
            accept_check_command(&mut child, &request, 1, deadline, &cancel, |_| panic!(
                "Invalid frame reached native authentication"
            ))
            .is_err(),
            "{damage}"
        );
    }
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

#[test]
fn public_pins_and_capture_roles_cannot_activate_the_child_native_load_boundary() {
    use std::os::unix::fs::DirBuilderExt;
    let root = std::env::temp_dir().canonicalize().unwrap().join(format!(
        "peerbrush-native-owner-gate-{}",
        uuid::Uuid::new_v4()
    ));
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(&root)
        .unwrap();
    let legacy = root.join("legacy");
    std::fs::create_dir(&legacy).unwrap();
    let profile = crate::settings::Profile::open(
        crate::settings::Locations {
            config: root.join("config"),
            data: root.join("data"),
            cache: root.join("cache"),
        },
        &legacy,
    )
    .unwrap();
    let before = std::fs::read(profile.locations.profile().join("profile.json")).unwrap();
    let owner = root.join("copy/PeerBrush.app");
    let target = root.join("original.app");
    let bundles = checkpoint::OwnerBundles {
        owner: owner.clone(),
        host: target.clone(),
        application: target.clone(),
    };
    let (channel, peer) = UnixStream::pair().unwrap();
    let configuration = ApprovedCheck {
        feed_url: crate::sparkle::PROPOSED_FEED_URL.into(),
        public_ed_key: "public fixture; not installation authority".into(),
        publisher_team_id: "ABCD123456".into(),
        objects: std::collections::BTreeMap::new(),
    };
    let error = NativeOwner::load(
        bundles,
        profile.update_profile(),
        channel,
        std::process::id(),
        &"a".repeat(64),
        configuration,
        Arc::new(AtomicBool::new(false)),
    )
    .err()
    .expect("Native ownership is source-disabled before bundle/API work");
    assert_eq!(error, checkpoint::require_captured_handoff().unwrap_err());
    assert_eq!(
        checkpoint::arm_owner_check(&owner, &"a".repeat(64), &profile.update_profile())
            .err()
            .unwrap(),
        checkpoint::require_captured_handoff().unwrap_err()
    );
    assert!(!owner.exists() && !target.exists());
    assert_eq!(
        std::fs::read(profile.locations.profile().join("profile.json")).unwrap(),
        before
    );
    assert!(!profile.locations.data.join("update-checkpoints").exists());
    drop(peer);
    drop(profile);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn download_command_and_reply_bind_exact_found_generation_on_the_actual_frame_path() {
    let request = request();
    let mut command = CheckCommand::new(&request, 2, CheckAction::Download);
    command.found_generation = Some(11);
    command.validate(&request, 2).unwrap();
    let (mut parent, mut child) = UnixStream::pair().unwrap();
    let deadline = Instant::now() + Duration::from_secs(2);
    let cancel = AtomicBool::new(false);
    send(&mut parent, &command, deadline, &cancel).unwrap();
    let authenticated = std::cell::Cell::new(false);
    let actual = accept_check_command(&mut child, &request, 2, deadline, &cancel, |_| {
        authenticated.set(true);
        Ok(())
    })
    .unwrap();
    assert!(authenticated.get());
    assert_eq!(actual.action, CheckAction::Download);
    assert_eq!(actual.found_generation, Some(11));
    let mut reply = CheckReply {
        binding: actual,
        owner_pid: 17,
        status: observation(),
        final_control: None,
    };
    reply
        .validate(&request, 17, 2, CheckAction::Download, Some(11), None)
        .unwrap();
    assert!(reply
        .validate(&request, 17, 2, CheckAction::Download, Some(12), None)
        .is_err());
    reply.binding.found_generation = Some(12);
    assert!(reply
        .validate(&request, 17, 2, CheckAction::Download, Some(11), None)
        .is_err());
    for damage in [
        "first",
        "missing",
        "zero",
        "status",
        "nonce",
        "checkpoint",
        "manifest",
    ] {
        let mut command = CheckCommand::new(&request, 2, CheckAction::Download);
        command.found_generation = Some(11);
        match damage {
            "first" => command.sequence = 1,
            "missing" => command.found_generation = None,
            "zero" => command.found_generation = Some(0),
            "status" => command.action = CheckAction::Status,
            "nonce" => command.nonce = "b".repeat(64),
            "checkpoint" => command.checkpoint = uuid::Uuid::new_v4().to_string(),
            "manifest" => command.manifest_sha256 = "c".repeat(64),
            _ => unreachable!(),
        }
        assert!(
            command.validate(&request, command.sequence).is_err(),
            "{damage}"
        );
    }
    assert!(checkpoint::require_captured_handoff().is_err());
}

#[test]
fn download_request_queue_is_one_shot_even_after_consumption_and_cancel_is_only_a_request() {
    let (_result, receiver) = mpsc::channel();
    let (requests, pending) = mpsc::sync_channel(1);
    let cancel = Arc::new(AtomicBool::new(false));
    let (_control_sender, final_control_reply) = mpsc::channel();
    let remote = RemoteCheck {
        final_control_reply,
        receiver,
        cancel: cancel.clone(),
        requests,
        download_requested: AtomicBool::new(false),
        final_profile_requested: AtomicBool::new(false),
        final_control_requested: AtomicBool::new(false),
    };
    assert!(remote.require_active().is_ok());
    assert!(remote.download(0).is_err());
    assert!(!remote.download_requested());
    remote.download(11).unwrap();
    assert!(remote.download_requested());
    assert!(matches!(
        pending.try_recv().unwrap(),
        OwnerRequest::Download(11)
    ));
    assert!(remote.download(11).is_err() && remote.download(12).is_err());
    assert!(matches!(pending.try_recv(), Err(mpsc::TryRecvError::Empty)));
    remote.cancel();
    assert!(remote.require_active().is_err());
    assert!(cancel.load(Ordering::Acquire));
    assert!(remote.download(11).is_err());
    let state = driver::State::default();
    state.session.borrow_mut().begin().unwrap();
    state.session.borrow_mut().checking();
    state.native_starting().unwrap();
    state.cancel();
    let mut late = observation();
    late.quarantine = false;
    late.phase = crate::sparkle::check::Phase::Found;
    late.held_generation = None;
    late.found_generation = Some(11);
    state.observe_owner(late);
    assert!(state.view().cancelled && state.view().native_cycle);
    assert_ne!(state.view().phase, crate::sparkle::check::Phase::Found);
    state.finished();
    assert!(state.view().quarantine && state.view().native_cycle);
    assert!(state.session.borrow_mut().begin().is_err());
}

#[test]
fn native_download_observations_reject_unbound_counters_and_invalid_extraction() {
    for damage in [
        "none", "zero", "counter", "expected", "extract", "nan", "infinite", "range",
    ] {
        let mut status = observation();
        status.download_generation = Some(11);
        status.expected_bytes = Some(4096);
        status.received_bytes = 2048;
        status.extraction = Some(0.5);
        status.download_complete = true;
        match damage {
            "zero" => status.download_generation = Some(0),
            "counter" => status.download_generation = None,
            "expected" => {
                status.download_generation = None;
                status.received_bytes = 0;
                status.extraction = None;
                status.download_complete = false;
            }
            "extract" => {
                status.download_generation = None;
                status.expected_bytes = None;
                status.received_bytes = 0;
                status.download_complete = false;
            }
            "nan" => status.extraction = Some(f64::NAN),
            "infinite" => status.extraction = Some(f64::INFINITY),
            "range" => status.extraction = Some(1.1),
            _ => {}
        }
        assert_eq!(status.validate().is_ok(), damage == "none", "{damage}");
    }
    let status = observation();
    assert!(serde_json::to_vec(&status).unwrap().len() <= FRAME_LIMIT);
    assert!(checkpoint::require_captured_handoff().is_err());
}

fn final_binding() -> checkpoint::FinalProfileBinding {
    checkpoint::FinalProfileBinding {
        ready_generation: 7,
        sources_sha256: "a".repeat(64),
        record_sha256: "b".repeat(64),
    }
}
#[test]
fn final_profile_command_and_reply_bind_actual_frames_to_ready_sources_record_and_owner() {
    let request = request();
    let binding = final_binding();
    let mut command = CheckCommand::new(&request, 4, CheckAction::ObserveFinalProfile);
    command.final_profile = Some(binding.clone());
    let (mut parent, mut child) = UnixStream::pair().unwrap();
    let deadline = Instant::now() + Duration::from_secs(2);
    let cancel = AtomicBool::new(false);
    send(&mut parent, &command, deadline, &cancel).unwrap();
    let authenticated = std::cell::Cell::new(false);
    let actual = accept_check_command(&mut child, &request, 4, deadline, &cancel, |_| {
        authenticated.set(true);
        Ok(())
    })
    .unwrap();
    assert!(authenticated.get());
    assert_eq!(actual.final_profile.as_ref(), Some(&binding));
    let mut status = observation();
    status.final_profile = Some(binding.clone());
    send(
        &mut child,
        &CheckReply {
            binding: actual,
            owner_pid: 17,
            status,
            final_control: None,
        },
        deadline,
        &cancel,
    )
    .unwrap();
    let reply: CheckReply = receive(&mut parent, deadline, &cancel).unwrap();
    reply
        .validate(
            &request,
            17,
            4,
            CheckAction::ObserveFinalProfile,
            None,
            Some(&binding),
        )
        .unwrap();
    for damage in [
        "first",
        "missing",
        "action",
        "found",
        "zero",
        "sources",
        "record",
        "uppercase",
        "nonce",
        "checkpoint",
        "manifest",
    ] {
        let mut command: CheckCommand =
            serde_json::from_slice(&serde_json::to_vec(&command).unwrap()).unwrap();
        match damage {
            "first" => command.sequence = 1,
            "missing" => command.final_profile = None,
            "action" => command.action = CheckAction::Status,
            "found" => command.found_generation = Some(3),
            "zero" => command.final_profile.as_mut().unwrap().ready_generation = 0,
            "sources" => command.final_profile.as_mut().unwrap().sources_sha256 = "a".repeat(63),
            "record" => {
                command.final_profile.as_mut().unwrap().record_sha256 = "not a digest".into()
            }
            "uppercase" => command.final_profile.as_mut().unwrap().record_sha256 = "B".repeat(64),
            "nonce" => command.nonce = "c".repeat(64),
            "checkpoint" => command.checkpoint = uuid::Uuid::new_v4().to_string(),
            "manifest" => command.manifest_sha256 = "d".repeat(64),
            _ => unreachable!(),
        }
        assert!(
            command.validate(&request, command.sequence).is_err(),
            "{damage}"
        );
    }
    for damage in [
        "owner",
        "ready",
        "sources",
        "record",
        "cancelled",
        "cycle",
        "quarantine",
    ] {
        let mut reply: CheckReply =
            serde_json::from_slice(&serde_json::to_vec(&reply).unwrap()).unwrap();
        match damage {
            "owner" => reply.owner_pid = 18,
            "ready" => reply.status.held_generation = Some(8),
            "sources" => {
                reply.binding.final_profile.as_mut().unwrap().sources_sha256 = "c".repeat(64)
            }
            "record" => {
                reply.binding.final_profile.as_mut().unwrap().record_sha256 = "d".repeat(64)
            }
            "cancelled" => reply.status.cancelled = true,
            "cycle" => reply.status.native_cycle = false,
            "quarantine" => reply.status.quarantine = false,
            _ => unreachable!(),
        }
        assert!(
            reply
                .validate(
                    &request,
                    17,
                    4,
                    CheckAction::ObserveFinalProfile,
                    None,
                    Some(&binding)
                )
                .is_err(),
            "{damage}"
        );
    }
    assert!(checkpoint::require_captured_handoff().is_err());
}
#[test]
fn final_profile_request_uses_the_sole_bounded_queue_once_and_retains_cancelled_or_failed_requests()
{
    for outcome in ["success", "full", "disconnected", "cancelled"] {
        let (_result, receiver) = mpsc::channel();
        let (requests, pending) = mpsc::sync_channel(1);
        let cancel = Arc::new(AtomicBool::new(false));
        let (_control_sender, final_control_reply) = mpsc::channel();
        let remote = RemoteCheck {
            final_control_reply,
            receiver,
            cancel: cancel.clone(),
            requests,
            download_requested: AtomicBool::new(false),
            final_profile_requested: AtomicBool::new(false),
            final_control_requested: AtomicBool::new(false),
        };
        if outcome == "full" {
            remote.download(3).unwrap();
        }
        if outcome == "cancelled" {
            remote.cancel();
        }
        let mut pending = Some(pending);
        if outcome == "disconnected" {
            drop(pending.take());
        }
        assert_eq!(
            remote.observe_final_profile(final_binding()).is_ok(),
            outcome == "success",
            "{outcome}"
        );
        if let Some(pending) = pending {
            match pending.try_recv() {
                Ok(OwnerRequest::ObserveFinalProfile(binding)) => {
                    assert_eq!(binding, final_binding())
                }
                Ok(OwnerRequest::Download(3)) => assert_eq!(outcome, "full"),
                Err(mpsc::TryRecvError::Empty) => assert_eq!(outcome, "cancelled"),
                _ => panic!("Wrong queued request"),
            }
        }
        assert!(
            remote.observe_final_profile(final_binding()).is_err(),
            "{outcome}"
        );
        remote.cancel();
        assert!(cancel.load(Ordering::Acquire));
        assert!(remote.observe_final_profile(final_binding()).is_err());
    }
}

#[test]
fn final_control_actual_frames_and_separate_delivery_bind_owner_attempt_sequence_ready_and_profile()
{
    let request = request();
    let binding = final_binding();
    let mut command = CheckCommand::new(&request, 5, CheckAction::PrepareFinalControl);
    command.final_profile = Some(binding.clone());
    let (mut parent, mut child) = UnixStream::pair().unwrap();
    let cancel = AtomicBool::new(false);
    let deadline = Instant::now() + Duration::from_secs(2);
    send(&mut parent, &command, deadline, &cancel).unwrap();
    let actual =
        accept_check_command(&mut child, &request, 5, deadline, &cancel, |_| Ok(())).unwrap();
    let mut status = observation();
    status.final_profile = Some(binding.clone());
    send(
        &mut child,
        &CheckReply {
            binding: actual,
            owner_pid: 17,
            status,
            final_control: Some(binding.clone()),
        },
        deadline,
        &cancel,
    )
    .unwrap();
    let raw = receive::<CheckReply>(&mut parent, deadline, &cancel).unwrap();
    raw.validate(
        &request,
        17,
        5,
        CheckAction::PrepareFinalControl,
        None,
        Some(&binding),
    )
    .unwrap();
    let encoded = serde_json::to_vec(&raw).unwrap();
    assert!(encoded.len() <= FRAME_LIMIT);
    let (observations, prior) = mpsc::sync_channel(1);
    let (control, acknowledgement) = mpsc::sync_channel(1);
    observations.send(Ok(observation())).unwrap();
    assert!(deliver_check_reply(raw, &observations, &control).unwrap());
    assert_eq!(acknowledgement.try_recv().unwrap().unwrap(), binding);
    assert!(
        prior.try_recv().unwrap().unwrap().final_profile.is_none(),
        "Old status remains separate from the exact acknowledgement"
    );
    for damage in [
        "owner",
        "sequence",
        "nonce",
        "attempt",
        "manifest",
        "action",
        "missing",
        "wrong-ready",
        "wrong-profile",
        "wrong-sources",
        "cancelled",
        "native-cycle",
    ] {
        let mut reply: CheckReply = serde_json::from_slice(&encoded).unwrap();
        match damage {
            "owner" => reply.owner_pid = 18,
            "sequence" => reply.binding.sequence = 6,
            "nonce" => reply.binding.nonce = "c".repeat(64),
            "attempt" => reply.binding.checkpoint = uuid::Uuid::new_v4().to_string(),
            "manifest" => reply.binding.manifest_sha256 = "d".repeat(64),
            "action" => reply.binding.action = CheckAction::ObserveFinalProfile,
            "missing" => reply.final_control = None,
            "wrong-ready" => reply.final_control.as_mut().unwrap().ready_generation += 1,
            "wrong-profile" => reply.final_control.as_mut().unwrap().record_sha256 = "c".repeat(64),
            "wrong-sources" => {
                reply.final_control.as_mut().unwrap().sources_sha256 = "d".repeat(64)
            }
            "cancelled" => reply.status.cancelled = true,
            "native-cycle" => reply.status.native_cycle = false,
            _ => unreachable!(),
        }
        assert!(
            reply
                .validate(
                    &request,
                    17,
                    5,
                    CheckAction::PrepareFinalControl,
                    None,
                    Some(&binding)
                )
                .is_err(),
            "{damage}"
        );
    }
    for sequence in [0, 1] {
        let mut early = CheckCommand::new(&request, sequence, CheckAction::PrepareFinalControl);
        early.final_profile = Some(binding.clone());
        assert!(early.validate(&request, sequence).is_err());
    }
    let first: CheckReply = serde_json::from_slice(&encoded).unwrap();
    assert!(deliver_check_reply(first, &observations, &control).unwrap());
    let duplicate: CheckReply = serde_json::from_slice(&encoded).unwrap();
    assert!(
        deliver_check_reply(duplicate, &observations, &control).is_err(),
        "Unread control cannot be overwritten"
    );
    assert_eq!(acknowledgement.try_recv().unwrap().unwrap(), binding);
    assert!(checkpoint::require_captured_handoff().is_err());
}
#[test]
fn final_control_request_keeps_once_ownership_after_queue_errors_consumption_and_late_cancel() {
    for outcome in ["success", "full", "disconnected", "cancelled"] {
        let (_result, receiver) = mpsc::channel();
        let (_ack, final_control_reply) = mpsc::channel();
        let (requests, pending) = mpsc::sync_channel(1);
        let cancel = Arc::new(AtomicBool::new(false));
        let remote = RemoteCheck {
            receiver,
            final_control_reply,
            cancel: cancel.clone(),
            requests,
            download_requested: AtomicBool::new(false),
            final_profile_requested: AtomicBool::new(false),
            final_control_requested: AtomicBool::new(false),
        };
        if outcome == "full" {
            remote.download(3).unwrap();
        }
        if outcome == "cancelled" {
            remote.cancel();
        }
        let mut pending = Some(pending);
        if outcome == "disconnected" {
            drop(pending.take());
        }
        assert_eq!(
            remote.prepare_final_control(final_binding()).is_ok(),
            outcome == "success",
            "{outcome}"
        );
        if let Some(pending) = pending {
            match pending.try_recv() {
                Ok(OwnerRequest::PrepareFinalControl(binding)) => {
                    assert_eq!(binding, final_binding())
                }
                Ok(OwnerRequest::Download(3)) => assert_eq!(outcome, "full"),
                Err(mpsc::TryRecvError::Empty) => assert_eq!(outcome, "cancelled"),
                _ => panic!("Wrong final-control request"),
            }
        }
        assert!(
            remote.prepare_final_control(final_binding()).is_err(),
            "{outcome}"
        );
        remote.cancel();
        assert!(remote.prepare_final_control(final_binding()).is_err());
        assert!(cancel.load(Ordering::Acquire));
    }
}
