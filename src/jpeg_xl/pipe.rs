//! Parent-only pipe cancellation: pump exit never depends on killing the child.
use std::{
    io::{self, Read, Write},
    process::{ChildStdin, ChildStdout},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    thread,
    time::Duration,
};

pub(super) struct Pipe<T> {
    inner: T,
    stop: Arc<AtomicBool>,
    #[cfg(test)]
    pub(super) progress: Option<Arc<std::sync::atomic::AtomicUsize>>,
}
impl<T> Pipe<T> {
    fn check(&self) -> io::Result<()> {
        if self.stop.load(Ordering::SeqCst) {
            Err(io::Error::other("JPEG XL pipe stopped"))
        } else {
            Ok(())
        }
    }
    fn pause(&self) -> io::Result<()> {
        self.check()?;
        thread::sleep(Duration::from_millis(1));
        self.check()
    }
}
#[cfg(unix)]
fn nonblocking(pipe: &impl std::os::fd::AsRawFd) -> io::Result<()> {
    let fd = pipe.as_raw_fd();
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}
impl Pipe<ChildStdin> {
    pub(super) fn writer(inner: ChildStdin, stop: Arc<AtomicBool>) -> io::Result<Self> {
        #[cfg(unix)]
        nonblocking(&inner)?;
        #[cfg(windows)]
        {
            use std::os::windows::io::AsRawHandle;
            use windows::Win32::{Foundation::HANDLE, System::Pipes::*};
            // Microsoft explicitly permits anonymous CreatePipe handles here.
            // Byte-mode NOWAIT may write a partial buffer or zero bytes; retry
            // both without ever using write_all on the underlying handle.
            // https://learn.microsoft.com/windows/win32/api/namedpipeapi/nf-namedpipeapi-setnamedpipehandlestate
            unsafe {
                SetNamedPipeHandleState(
                    HANDLE(inner.as_raw_handle()),
                    Some(&PIPE_NOWAIT),
                    None,
                    None,
                )
            }
            .map_err(|_| io::Error::last_os_error())?;
        }
        Ok(Self {
            inner,
            stop,
            #[cfg(test)]
            progress: None,
        })
    }
}
impl Pipe<ChildStdout> {
    pub(super) fn reader(inner: ChildStdout, stop: Arc<AtomicBool>) -> io::Result<Self> {
        #[cfg(unix)]
        nonblocking(&inner)?;
        Ok(Self {
            inner,
            stop,
            #[cfg(test)]
            progress: None,
        })
    }
}
impl Read for Pipe<ChildStdout> {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        if bytes.is_empty() {
            return Ok(0);
        }
        loop {
            self.check()?;
            let count = bytes.len();
            #[cfg(windows)]
            let count = {
                use std::os::windows::io::AsRawHandle;
                use windows::Win32::{
                    Foundation::{ERROR_BROKEN_PIPE, ERROR_PIPE_NOT_CONNECTED, HANDLE},
                    System::Pipes::PeekNamedPipe,
                };
                let mut available = 0;
                if unsafe {
                    PeekNamedPipe(
                        HANDLE(self.inner.as_raw_handle()),
                        None,
                        0,
                        None,
                        Some(&mut available),
                        None,
                    )
                }
                .is_err()
                {
                    let error = io::Error::last_os_error();
                    if matches!(error.raw_os_error(), Some(code) if code == ERROR_BROKEN_PIPE.0 as i32 || code == ERROR_PIPE_NOT_CONNECTED.0 as i32)
                    {
                        return Ok(0);
                    }
                    return Err(error);
                }
                if available == 0 {
                    self.pause()?;
                    continue;
                }
                // This is the pipe's only reader; exactly the available bytes
                // can be read synchronously without waiting for future output.
                count.min(available as usize)
            };
            match self.inner.read(&mut bytes[..count]) {
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::Interrupted | io::ErrorKind::WouldBlock
                    ) =>
                {
                    self.pause()?
                }
                result => return result,
            }
        }
    }
}
impl Write for Pipe<ChildStdin> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.is_empty() {
            return Ok(0);
        }
        loop {
            self.check()?;
            match self.inner.write(bytes) {
                Ok(0) => self.pause()?,
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::Interrupted | io::ErrorKind::WouldBlock
                    ) =>
                {
                    self.pause()?
                }
                #[cfg(test)]
                Ok(count) => {
                    if let Some(progress) = &self.progress {
                        progress.fetch_add(count, Ordering::SeqCst);
                    }
                    return Ok(count);
                }
                result => return result,
            }
        }
    }
    fn flush(&mut self) -> io::Result<()> {
        self.check()
    }
}
