use super::*;
use crate::large_document::ResidentBudget;
use std::io::{Read, Write};
use std::sync::atomic::AtomicUsize;
static SERIAL: Mutex<()> = Mutex::new(());
const HELPER: &str = "PEERBRUSH_CODE_PROCESS_FIXTURE";

// Only an explicitly spawned test process sleeps. The ordinary test entry
// returns without changing the test runner's resource limits or environment.
#[test]
fn helper() {
    if std::env::var(HELPER).as_deref() == Ok("silent") {
        thread::sleep(Duration::from_secs(60));
    }
}
fn fixture(arguments: &[&str]) -> Command {
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args(arguments)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000);
    }
    command
}
fn silent() -> Command {
    let mut command = fixture(&[
        "--exact",
        "code::worker::process::tests::helper",
        "--nocapture",
    ]);
    command.env(HELPER, "silent");
    command
}
fn pool() -> ResidentBudget {
    ResidentBudget::new(OWNED_BYTES + 1024 * 1024).unwrap()
}
fn charge(pool: &ResidentBudget) -> Arc<MetadataCharge> {
    MetadataCharge::reserve(pool, OWNED_BYTES, &|| Ok(())).unwrap()
}
fn refunded(pool: &ResidentBudget) {
    let until = Instant::now() + Duration::from_secs(5);
    while pool.used() != 0 {
        assert!(
            Instant::now() < until,
            "Child admission did not retire after confirmed exit"
        );
        WAKE.notify_one();
        thread::sleep(Duration::from_millis(10));
    }
}
fn deadline(started: Instant, duration: Duration) -> Result<()> {
    if started.elapsed() >= duration {
        Err("Fixture deadline".into())
    } else {
        Ok(())
    }
}

#[cfg(any(windows, target_os = "linux"))]
#[test]
fn transport_result_requires_actual_successful_exit_and_releases_known_child_owners() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let pool = pool();
    let started = Instant::now();
    let result = supervise(
        fixture(&["--list"]),
        charge(&pool),
        &|| deadline(started, Duration::from_secs(5)),
        |mut reader, writer| {
            drop(writer);
            std::io::copy(&mut reader, &mut std::io::sink()).map_err(|e| e.to_string())?;
            Ok(17)
        },
    )
    .unwrap();
    assert_eq!(result, 17);
    assert_eq!(pool.used(), 0);
    refunded(&pool);
    let started = Instant::now();
    let result = supervise(
        fixture(&["--peerbrush-invalid-test-argument"]),
        charge(&pool),
        &|| deadline(started, Duration::from_secs(5)),
        |_, _| Ok(17),
    );
    assert!(result.is_err());
    refunded(&pool);
    let started = Instant::now();
    let result = supervise(
        silent(),
        charge(&pool),
        &|| deadline(started, Duration::from_millis(200)),
        |_, _| Ok(17),
    );
    assert!(matches!(result, Err(error) if error == "Fixture deadline"));
    refunded(&pool);
}

#[cfg(any(windows, target_os = "linux"))]
#[test]
fn cancellation_and_watchdog_panic_interrupt_blocked_pipes_before_scope_joins() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    for panic in [false, true] {
        let pool = pool();
        let started = Instant::now();
        let checks = AtomicUsize::new(0);
        let check = || {
            if checks.fetch_add(1, Ordering::SeqCst) >= 3 {
                if panic {
                    panic!("Fixture watchdog panic");
                }
                return Err("Fixture canceled".into());
            }
            deadline(started, Duration::from_secs(5))
        };
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            supervise(silent(), charge(&pool), &check, |mut reader, mut writer| {
                // A small input fits even when the fixture never consumes it.
                writer.write_all(b"waiting").map_err(|e| e.to_string())?;
                let mut bytes = [0; 1024];
                reader.read_exact(&mut bytes).map_err(|e| e.to_string())?;
                Ok(17)
            })
        }));
        if panic {
            assert!(result.is_err());
        } else {
            assert!(result.unwrap().is_err());
        }
        assert!(started.elapsed() < Duration::from_secs(5));
        refunded(&pool);
    }
}

#[cfg(any(windows, target_os = "linux"))]
#[test]
fn unconfirmed_stop_retains_actual_child_and_charge_until_the_last_controller_releases() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let pool = pool();
    let mut guard = Guard::launch(silent(), charge(&pool)).unwrap();
    let pipes = guard.pipes().unwrap();
    let owner = guard.owner.clone();
    let held = pool.used();
    // Simulate an OS stop refusal after the stop path has run. Keeping another
    // actual controller Arc prevents the reaper from claiming sole ownership.
    guard.stop.store(true, Ordering::SeqCst);
    guard.attempted_stop = true;
    drop(guard);
    thread::sleep(Duration::from_millis(100));
    assert_eq!(pool.used(), held);
    assert!(owner.lock().unwrap().child.try_wait().unwrap().is_none());
    assert!(CHILDREN
        .lock()
        .unwrap()
        .iter()
        .flatten()
        .any(|slot| Arc::ptr_eq(slot, &owner)));
    drop(pipes);
    drop(owner);
    refunded(&pool);
}

#[cfg(any(windows, target_os = "linux"))]
#[test]
fn refusal_before_launch_never_starts_an_exchange_or_retains_child_capacity() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let pool = pool();
    let entered = AtomicBool::new(false);
    let result = supervise(
        fixture(&["--list"]),
        charge(&pool),
        &|| Err("Canceled".into()),
        |_, _| {
            entered.store(true, Ordering::SeqCst);
            Ok(17)
        },
    );
    assert!(result.is_err());
    assert!(!entered.load(Ordering::SeqCst));
    assert_eq!(pool.used(), 0);
}
