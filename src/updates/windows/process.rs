//! Kernel process objects, not PIDs, establish lifetime and peer identity.
use super::*;
use ::windows::{
    core::PWSTR,
    Win32::{
        Foundation::{FILETIME, HANDLE, WAIT_OBJECT_0, WAIT_TIMEOUT},
        System::Threading::{
            GetProcessId, GetProcessTimes, OpenProcess, QueryFullProcessImageNameW,
            WaitForSingleObject, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
            PROCESS_SYNCHRONIZE,
        },
    },
};
use std::os::windows::io::{AsRawHandle, BorrowedHandle, FromRawHandle, OwnedHandle};

pub(super) struct Process {
    handle: OwnedHandle,
    pub(super) pid: u32,
    pub(super) created: u64,
}
impl Process {
    pub(super) fn open(pid: u32) -> Result<Self> {
        if pid == 0 {
            return Err(STAGE_HOLD.into());
        }
        let raw = unsafe {
            OpenProcess(
                PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE,
                false,
                pid,
            )
        }
        .map_err(|_| STAGE_HOLD)?;
        let handle = unsafe { OwnedHandle::from_raw_handle(raw.0) };
        Self::bind(handle, pid)
    }
    pub(super) fn child(child: &std::process::Child) -> Result<Self> {
        let handle = unsafe { BorrowedHandle::borrow_raw(child.as_raw_handle()) }
            .try_clone_to_owned()
            .map_err(|_| STAGE_HOLD)?;
        Self::bind(handle, child.id())
    }
    fn bind(handle: OwnedHandle, pid: u32) -> Result<Self> {
        let raw = HANDLE(handle.as_raw_handle());
        if unsafe { GetProcessId(raw) } != pid {
            return Err(STAGE_HOLD.into());
        }
        let mut created = FILETIME::default();
        let mut exited = FILETIME::default();
        let mut kernel = FILETIME::default();
        let mut user = FILETIME::default();
        unsafe { GetProcessTimes(raw, &mut created, &mut exited, &mut kernel, &mut user) }
            .map_err(|_| STAGE_HOLD)?;
        let created = u64::from(created.dwLowDateTime) | (u64::from(created.dwHighDateTime) << 32);
        if created == 0 {
            return Err(STAGE_HOLD.into());
        }
        Ok(Self {
            handle,
            pid,
            created,
        })
    }
    pub(super) fn handle(&self) -> HANDLE {
        HANDLE(self.handle.as_raw_handle())
    }
    pub(super) fn exited(&self) -> Result<bool> {
        match unsafe { WaitForSingleObject(self.handle(), 0) } {
            WAIT_OBJECT_0 => Ok(true),
            WAIT_TIMEOUT => Ok(false),
            _ => Err(STAGE_HOLD.into()),
        }
    }
    pub(super) fn image(&self) -> Result<PathBuf> {
        let mut text = vec![0; 32768];
        let mut count = text.len() as u32;
        unsafe {
            QueryFullProcessImageNameW(
                self.handle(),
                PROCESS_NAME_WIN32,
                PWSTR(text.as_mut_ptr()),
                &mut count,
            )
        }
        .map_err(|_| STAGE_HOLD)?;
        use std::os::windows::ffi::OsStringExt;
        if count == 0 || count as usize >= text.len() {
            return Err(STAGE_HOLD.into());
        }
        Ok(PathBuf::from(std::ffi::OsString::from_wide(
            &text[..count as usize],
        )))
    }
    pub(super) fn authenticate(
        &self,
        executable: &Path,
        target: &Target,
        control: &Control,
    ) -> Result<()> {
        use ::windows::Win32::Storage::Packaging::Appx::GetPackageFullName;
        if self.exited()?
            || fs::canonicalize(self.image()?).map_err(|_| STAGE_HOLD)?
                != fs::canonicalize(executable).map_err(|_| STAGE_HOLD)?
        {
            return Err(STAGE_HOLD.into());
        }
        let held = Held::open(executable, PACKAGE, control)?;
        if held.media != target.executable {
            return Err(STAGE_HOLD.into());
        }
        let mut count = 0;
        let first = unsafe { GetPackageFullName(self.handle(), &mut count, PWSTR::null()) };
        if first.0 != 122 || !(2..=1024).contains(&count) {
            return Err(STAGE_HOLD.into());
        }
        let mut text = vec![0; count as usize];
        if unsafe { GetPackageFullName(self.handle(), &mut count, PWSTR(text.as_mut_ptr())) }.0 != 0
            || count < 2
            || count as usize > text.len()
            || text[count as usize - 1] != 0
            || String::from_utf16(&text[..count as usize - 1]).map_err(|_| STAGE_HOLD)?
                != target.full_name
        {
            return Err(STAGE_HOLD.into());
        }
        held.check(control)?;
        if self.exited()? {
            return Err(STAGE_HOLD.into());
        }
        Ok(())
    }
}
