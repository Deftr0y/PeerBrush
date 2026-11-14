//! Child-local limits are established before accepting script/metadata bytes.
//! The parent separately owns forced teardown and child-memory admission.
const ERROR: &str = "Code worker resource protection is unavailable";
pub(super) const MEMORY: usize = 512 * 1024 * 1024;
pub(super) struct Permit {
    #[cfg(windows)]
    job: Option<windows::Win32::Foundation::HANDLE>,
}
#[cfg(windows)]
impl Drop for Permit {
    fn drop(&mut self) {
        if let Some(job) = self.job.take() {
            let _ = unsafe { windows::Win32::Foundation::CloseHandle(job) };
        }
    }
}
pub(super) fn acquire() -> Result<Permit, String> {
    #[cfg(windows)]
    {
        use windows::Win32::System::{
            Diagnostics::Debug::*, ErrorReporting::*, JobObjects::*, Threading::GetCurrentProcess,
        };
        unsafe {
            SetErrorMode(SEM_FAILCRITICALERRORS | SEM_NOGPFAULTERRORBOX);
            WerSetFlags(WER_FAULT_REPORTING(WER_FAULT_REPORTING_NO_UI)).map_err(|_| ERROR)?;
        }
        let permit = job(false)?;
        unsafe {
            AssignProcessToJobObject(permit.job.ok_or(ERROR)?, GetCurrentProcess())
                .map_err(|_| ERROR)?;
        }
        // The child owns no KILL_ON_JOB_CLOSE handle: normal return must be an
        // ordinary exit. The parent supervisor owns forced teardown.
        Ok(permit)
    }
    #[cfg(target_os = "linux")]
    {
        for (resource, maximum) in [
            (libc::RLIMIT_CORE, 0),
            (libc::RLIMIT_CPU, 30),
            (libc::RLIMIT_AS, MEMORY),
        ] {
            let mut current = std::mem::MaybeUninit::<libc::rlimit>::uninit();
            if unsafe { libc::getrlimit(resource, current.as_mut_ptr()) } != 0 {
                return Err(ERROR.into());
            }
            let current = unsafe { current.assume_init() };
            let limit = libc::rlimit {
                rlim_cur: current.rlim_cur.min(maximum as libc::rlim_t),
                rlim_max: current.rlim_max.min(maximum as libc::rlim_t),
            };
            if unsafe { libc::setrlimit(resource, &limit) } != 0 {
                return Err(ERROR.into());
            }
        }
        let parent = unsafe { libc::getppid() };
        if parent <= 1
            || unsafe { libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) } != 0
            || unsafe { libc::getppid() } != parent
        {
            return Err(ERROR.into());
        }
        Ok(Permit {})
    }
    #[cfg(not(any(windows, target_os = "linux")))]
    {
        // No physical-memory sampling is represented as a hard allocation cap.
        // Native macOS/other-platform execution stays unavailable until qualified.
        Err(ERROR.into())
    }
}
#[cfg(windows)]
fn job(kill_on_close: bool) -> Result<Permit, String> {
    use windows::{core::PCWSTR, Win32::System::JobObjects::*};
    let job = unsafe { CreateJobObjectW(None, PCWSTR::null()) }.map_err(|_| ERROR)?;
    let permit = Permit { job: Some(job) };
    let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
    limits.BasicLimitInformation.LimitFlags =
        JOB_OBJECT_LIMIT_PROCESS_MEMORY | JOB_OBJECT_LIMIT_ACTIVE_PROCESS;
    if kill_on_close {
        limits.BasicLimitInformation.LimitFlags |= JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
    }
    limits.BasicLimitInformation.ActiveProcessLimit = 1;
    limits.ProcessMemoryLimit = MEMORY;
    unsafe {
        SetInformationJobObject(
            job,
            JobObjectExtendedLimitInformation,
            std::ptr::from_ref(&limits).cast(),
            std::mem::size_of_val(&limits) as u32,
        )
        .map_err(|_| ERROR)?;
    }
    Ok(permit)
}

pub(super) struct Parent {
    #[cfg(windows)]
    permit: Permit,
}
// The parent exclusively owns this unnamed job handle; only moves cross threads.
#[cfg(windows)]
unsafe impl Send for Parent {}
impl Parent {
    pub(super) fn supported() -> Result<(), String> {
        if cfg!(any(windows, target_os = "linux")) {
            Ok(())
        } else {
            Err(ERROR.into())
        }
    }
    pub(super) fn new(child: &std::process::Child) -> Result<Self, String> {
        Self::supported()?;
        #[cfg(windows)]
        {
            use std::os::windows::io::AsRawHandle;
            use windows::Win32::{
                Foundation::HANDLE, System::JobObjects::AssignProcessToJobObject,
            };
            let permit = job(true)?;
            unsafe {
                AssignProcessToJobObject(permit.job.ok_or(ERROR)?, HANDLE(child.as_raw_handle()))
            }
            .map_err(|_| ERROR)?;
            return Ok(Self { permit });
        }
        #[cfg(not(windows))]
        {
            let _ = child;
            Ok(Self {})
        }
    }
    #[cfg(windows)]
    pub(super) fn terminate(&self) {
        if let Some(job) = self.permit.job {
            let _ = unsafe { windows::Win32::System::JobObjects::TerminateJobObject(job, 2) };
        }
    }
}
#[cfg(test)]
impl Permit {
    pub(super) fn for_test() -> Self {
        Self {
            #[cfg(windows)]
            job: None,
        }
    }
}
