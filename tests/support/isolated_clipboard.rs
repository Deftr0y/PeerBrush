//! Real Windows transport tests run in a disposable, noninteractive child process.
//! Each window station owns a separate clipboard. The interactive clipboard is
//! never opened, copied or overwritten by these tests.
use std::{ffi::c_void, ptr};
type Handle = *mut c_void;
#[repr(C)]
struct Message {
    window: Handle,
    kind: u32,
    wparam: usize,
    lparam: isize,
    time: u32,
    point: [i32; 2],
    private: u32,
}
#[link(name = "user32")]
extern "system" {
    fn CreateWindowStationW(name: *const u16, flags: u32, access: u32, security: Handle) -> Handle;
    fn GetUserObjectInformationW(
        object: Handle,
        index: i32,
        out: Handle,
        bytes: u32,
        needed: *mut u32,
    ) -> i32;
    fn SetProcessWindowStation(station: Handle) -> i32;
    fn CreateDesktopW(
        name: *const u16,
        device: *const u16,
        mode: Handle,
        flags: u32,
        access: u32,
        security: Handle,
    ) -> Handle;
    fn SetThreadDesktop(desktop: Handle) -> i32;
    fn CreateWindowExW(
        ex: u32,
        class: *const u16,
        name: *const u16,
        style: u32,
        x: i32,
        y: i32,
        width: i32,
        height: i32,
        parent: Handle,
        menu: Handle,
        instance: Handle,
        param: Handle,
    ) -> Handle;
    fn DestroyWindow(window: Handle) -> i32;
    fn PeekMessageW(message: *mut Message, window: Handle, min: u32, max: u32, remove: u32) -> i32;
    fn TranslateMessage(message: *const Message) -> i32;
    fn DispatchMessageW(message: *const Message) -> isize;
}
pub struct Station {
    // Keep the private station/desktop alive until this dedicated child exits.
    // Windows closes their handles when the child ends. It never reconnects to
    // the interactive station while clipboard worker threads still exist.
    _station: Handle,
    _desktop: Handle,
    window: Handle,
}
impl Station {
    pub fn new() -> Self {
        assert_eq!(
            std::env::var("PEERBRUSH_CLIPBOARD_TEST_STATION").as_deref(),
            Ok("1")
        );
        unsafe {
            let station = CreateWindowStationW(ptr::null(), 0, 0x10000000, ptr::null_mut());
            assert!(
                !station.is_null(),
                "CreateWindowStation: {}",
                std::io::Error::last_os_error()
            );
            let mut name = [0u16; 256];
            let mut needed = 0;
            assert_ne!(
                GetUserObjectInformationW(station, 2, name.as_mut_ptr().cast(), 512, &mut needed),
                0
            );
            let len = name.iter().position(|c| *c == 0).unwrap();
            assert_ne!(
                String::from_utf16_lossy(&name[..len]).to_lowercase(),
                "winsta0",
                "Refuse the interactive clipboard"
            );
            assert_ne!(SetProcessWindowStation(station), 0);
            let desktop_name = format!("PeerBrushClipboardQa{}", uuid::Uuid::new_v4().simple())
                .encode_utf16()
                .chain([0])
                .collect::<Vec<_>>();
            let desktop = CreateDesktopW(
                desktop_name.as_ptr(),
                ptr::null(),
                ptr::null_mut(),
                0,
                0x10000000,
                ptr::null_mut(),
            );
            assert!(
                !desktop.is_null(),
                "CreateDesktop: {}",
                std::io::Error::last_os_error()
            );
            assert_ne!(SetThreadDesktop(desktop), 0);
            assert!(
                clipboard_win::count_formats().unwrap_or(0) == 0,
                "Refuse a nonempty noninteractive clipboard"
            );
            let class = "STATIC".encode_utf16().chain([0]).collect::<Vec<_>>();
            let window = CreateWindowExW(
                0,
                class.as_ptr(),
                class.as_ptr(),
                0,
                0,
                0,
                1,
                1,
                ptr::null_mut(),
                ptr::null_mut(),
                ptr::null_mut(),
                ptr::null_mut(),
            );
            assert!(
                !window.is_null(),
                "CreateWindow: {}",
                std::io::Error::last_os_error()
            );
            Self {
                _station: station,
                _desktop: desktop,
                window,
            }
        }
    }
    pub fn set_raw(&self, format: u32, bytes: &[u8]) {
        let _open = clipboard_win::Clipboard::new_for(self.window).unwrap();
        clipboard_win::raw::set(format, bytes).unwrap();
    }
    pub fn receive<T>(&self, receiver: &std::sync::mpsc::Receiver<T>) -> T {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            // Windows sends clipboard-owner notifications synchronously across
            // threads. Pump this hidden owner's messages while the worker runs.
            unsafe {
                let mut message: Message = std::mem::zeroed();
                while PeekMessageW(&mut message, ptr::null_mut(), 0, 0, 1) != 0 {
                    TranslateMessage(&message);
                    DispatchMessageW(&message);
                }
            }
            match receiver.recv_timeout(std::time::Duration::from_millis(5)) {
                Ok(value) => return value,
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => assert!(
                    std::time::Instant::now() < deadline,
                    "Clipboard worker timed out"
                ),
                Err(error) => panic!("Clipboard worker disconnected: {error}"),
            }
        }
    }
}
impl Drop for Station {
    fn drop(&mut self) {
        unsafe {
            DestroyWindow(self.window);
        }
    }
}

pub fn child(test: &str) -> bool {
    if std::env::var("PEERBRUSH_CLIPBOARD_TEST_STATION").as_deref() == Ok("1") {
        return false;
    }
    let result = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", test, "--nocapture", "--test-threads=1"])
        .env("PEERBRUSH_CLIPBOARD_TEST_STATION", "1")
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "Isolated clipboard child failed:\n{}\n{}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
    true
}
