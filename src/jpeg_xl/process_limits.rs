//! Child-only limits and parent-owned lifetime. macOS footprint sampling is
//! explicitly not a hard resident-memory ceiling.
use std::process::Child;
const MEMORY: usize = 1536 * 1024 * 1024;
const ERROR: &str = "JPEG XL worker resource protection is unavailable or exceeded";
#[derive(Clone, Copy)]
pub(super) enum Failure {
    Unavailable,
    Exceeded,
}
impl Failure {
    pub(super) fn rejects(self, exited: bool) -> bool {
        matches!(self, Self::Exceeded) || !exited
    }
    pub(super) fn message(self) -> String {
        ERROR.into()
    }
}

#[cfg(windows)]
pub(super) struct Parent(windows::Win32::Foundation::HANDLE);
#[cfg(not(windows))]
pub(super) struct Parent;

impl Parent {
    #[cfg(windows)]
    pub(super) fn terminate(&self) {
        let _ = unsafe { windows::Win32::System::JobObjects::TerminateJobObject(self.0, 2) };
        // The owner is then dropped: KILL_ON_JOB_CLOSE is the second path.
    }
    pub(super) fn new(child: &Child) -> Result<Self, String> {
        #[cfg(windows)]
        {
            use std::os::windows::io::AsRawHandle;
            use windows::{
                core::PCWSTR,
                Win32::{Foundation::HANDLE, System::JobObjects::*},
            };
            let job = unsafe { CreateJobObjectW(None, PCWSTR::null()) }.map_err(|_| ERROR)?;
            let owner = Self(job);
            let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
            limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE
                | JOB_OBJECT_LIMIT_PROCESS_MEMORY
                | JOB_OBJECT_LIMIT_ACTIVE_PROCESS;
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
                AssignProcessToJobObject(job, HANDLE(child.as_raw_handle())).map_err(|_| ERROR)?;
            }
            Ok(owner)
        }
        #[cfg(not(windows))]
        {
            check(child).map_err(Failure::message)?;
            Ok(Self)
        }
    }
}
#[cfg(windows)]
impl Drop for Parent {
    fn drop(&mut self) {
        let _ = unsafe { windows::Win32::Foundation::CloseHandle(self.0) };
    }
}
pub(super) fn check(child: &Child) -> Result<(), Failure> {
    #[cfg(target_os = "macos")]
    {
        let pid = i32::try_from(child.id()).map_err(|_| Failure::Unavailable)?;
        let mut usage = std::mem::MaybeUninit::<libc::rusage_info_v2>::zeroed();
        let status =
            unsafe { libc::proc_pid_rusage(pid, libc::RUSAGE_INFO_V2, usage.as_mut_ptr().cast()) };
        if status != 0 {
            return Err(Failure::Unavailable);
        }
        if unsafe { usage.assume_init() }.ri_phys_footprint > MEMORY as u64 {
            return Err(Failure::Exceeded);
        }
    }
    #[cfg(not(target_os = "macos"))]
    let _ = child;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn measured_resource_excess_is_never_ignored_after_exit() {
        assert!(Failure::Exceeded.rejects(false));
        assert!(Failure::Exceeded.rejects(true));
        assert!(Failure::Unavailable.rejects(false));
        assert!(!Failure::Unavailable.rejects(true));
    }
}
pub(super) fn worker() -> Result<(), String> {
    #[cfg(windows)]
    unsafe {
        use windows::Win32::System::{Diagnostics::Debug::*, ErrorReporting::*};
        // Child-local runtime behavior only: faults must not open a dialog or
        // hang the parent waiting for an invisible error-reporting prompt.
        SetErrorMode(SEM_FAILCRITICALERRORS | SEM_NOGPFAULTERRORBOX);
        WerSetFlags(WER_FAULT_REPORTING(WER_FAULT_REPORTING_NO_UI)).map_err(|_| ERROR)?;
    }
    #[cfg(unix)]
    {
        macro_rules! limit {
            ($resource:expr, $value:expr) => {{
                let value = $value as libc::rlim_t;
                let mut current = std::mem::MaybeUninit::<libc::rlimit>::uninit();
                if unsafe { libc::getrlimit($resource, current.as_mut_ptr()) } != 0 {
                    return Err(ERROR.into());
                }
                let current = unsafe { current.assume_init() };
                let setting = libc::rlimit {
                    rlim_cur: current.rlim_cur.min(value),
                    rlim_max: current.rlim_max.min(value),
                };
                if unsafe { libc::setrlimit($resource, &setting) } != 0 {
                    return Err(ERROR.into());
                }
            }};
        }
        limit!(libc::RLIMIT_CORE, 0);
        limit!(libc::RLIMIT_CPU, 120);
        #[cfg(target_os = "linux")]
        {
            limit!(libc::RLIMIT_AS, MEMORY);
            let parent = unsafe { libc::getppid() };
            if parent <= 1
                || unsafe { libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) } != 0
                || unsafe { libc::getppid() } != parent
            {
                return Err(ERROR.into());
            }
        }
        #[cfg(target_os = "macos")]
        // RLIMIT_AS aliases advisory RLIMIT_RSS on Darwin. Parent samples the
        // physical footprint; do not claim setrlimit is a hard allocation cap.
        limit!(libc::RLIMIT_RSS, MEMORY);
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        return Err(ERROR.into());
    }
    #[cfg(not(any(unix, windows)))]
    return Err(ERROR.into());
    Ok(())
}
