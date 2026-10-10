//! Preserve access rules before writing any document bytes to a new file.
#[cfg(unix)]
use std::fs;
use std::path::Path;

#[cfg(windows)]
pub(crate) fn preserve(source: &Path, destination: &Path) -> Result<(), String> {
    use std::{ffi::c_void, os::windows::ffi::OsStrExt, ptr::null_mut};
    #[link(name = "advapi32")]
    extern "system" {
        fn GetNamedSecurityInfoW(
            name: *const u16,
            kind: u32,
            info: u32,
            owner: *mut *mut c_void,
            group: *mut *mut c_void,
            dacl: *mut *mut c_void,
            sacl: *mut *mut c_void,
            descriptor: *mut *mut c_void,
        ) -> u32;
        fn SetNamedSecurityInfoW(
            name: *mut u16,
            kind: u32,
            info: u32,
            owner: *mut c_void,
            group: *mut c_void,
            dacl: *mut c_void,
            sacl: *mut c_void,
        ) -> u32;
    }
    #[link(name = "kernel32")]
    extern "system" {
        fn LocalFree(memory: *mut c_void) -> *mut c_void;
    }
    let source: Vec<u16> = source.as_os_str().encode_wide().chain(Some(0)).collect();
    let mut destination: Vec<u16> = destination
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect();
    let mut dacl = null_mut();
    let mut descriptor = null_mut();
    let error = unsafe {
        GetNamedSecurityInfoW(
            source.as_ptr(),
            1,
            4,
            null_mut(),
            null_mut(),
            &mut dacl,
            null_mut(),
            &mut descriptor,
        )
    };
    if error != 0 {
        return Err(format!(
            "Cannot preserve file access rules: {}",
            std::io::Error::from_raw_os_error(error as i32)
        ));
    }
    // Freeze the copied rules, preventing the backup directory from adding grants.
    let error = unsafe {
        SetNamedSecurityInfoW(
            destination.as_mut_ptr(),
            1,
            4 | 0x80000000,
            null_mut(),
            null_mut(),
            dacl,
            null_mut(),
        )
    };
    unsafe {
        LocalFree(descriptor);
    }
    if error != 0 {
        return Err(format!(
            "Cannot preserve file access rules: {}",
            std::io::Error::from_raw_os_error(error as i32)
        ));
    }
    Ok(())
}

#[cfg(unix)]
pub(crate) fn preserve(source: &Path, destination: &Path) -> Result<(), String> {
    use std::os::unix::{fs::MetadataExt, io::AsRawFd};
    extern "C" {
        fn fchown(fd: i32, owner: u32, group: u32) -> i32;
    }
    let source = fs::File::open(source).map_err(|e| e.to_string())?;
    let destination = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(destination)
        .map_err(|e| e.to_string())?;
    let before = source.metadata().map_err(|e| e.to_string())?;
    let current = destination.metadata().map_err(|e| e.to_string())?;
    if (before.uid(), before.gid()) != (current.uid(), current.gid())
        && unsafe { fchown(destination.as_raw_fd(), before.uid(), before.gid()) } != 0
    {
        return Err(format!(
            "Cannot preserve file ownership; use a new Save As path: {}",
            std::io::Error::last_os_error()
        ));
    }
    destination
        .set_permissions(before.permissions())
        .map_err(|e| e.to_string())?;
    copy_acl(&source, &destination)
}

#[cfg(target_os = "linux")]
fn copy_acl(source: &fs::File, destination: &fs::File) -> Result<(), String> {
    use std::{ffi::c_void, os::unix::io::AsRawFd};
    extern "C" {
        fn fgetxattr(fd: i32, name: *const i8, value: *mut c_void, size: usize) -> isize;
        fn fsetxattr(
            fd: i32,
            name: *const i8,
            value: *const c_void,
            size: usize,
            flags: i32,
        ) -> i32;
        fn fremovexattr(fd: i32, name: *const i8) -> i32;
    }
    let name = b"system.posix_acl_access\0";
    let size = unsafe {
        fgetxattr(
            source.as_raw_fd(),
            name.as_ptr().cast(),
            std::ptr::null_mut(),
            0,
        )
    };
    if size < 0 {
        let error = std::io::Error::last_os_error();
        if !matches!(error.raw_os_error(), Some(61 | 95)) {
            return Err(error.to_string());
        }
        if unsafe { fremovexattr(destination.as_raw_fd(), name.as_ptr().cast()) } != 0 {
            let error = std::io::Error::last_os_error();
            if !matches!(error.raw_os_error(), Some(61 | 95)) {
                return Err(error.to_string());
            }
        }
        return Ok(());
    }
    if size > 64 * 1024 {
        return Err("File access rules exceed their safety limit".into());
    }
    let mut acl = vec![0u8; size as usize];
    if unsafe {
        fgetxattr(
            source.as_raw_fd(),
            name.as_ptr().cast(),
            acl.as_mut_ptr().cast(),
            acl.len(),
        )
    } != size
    {
        return Err("File access rules changed while preparing save".into());
    }
    if unsafe {
        fsetxattr(
            destination.as_raw_fd(),
            name.as_ptr().cast(),
            acl.as_ptr().cast(),
            acl.len(),
            0,
        )
    } != 0
    {
        return Err(std::io::Error::last_os_error().to_string());
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn copy_acl(source: &fs::File, destination: &fs::File) -> Result<(), String> {
    use std::{ffi::c_void, os::unix::io::AsRawFd};
    extern "C" {
        fn acl_get_fd_np(fd: i32, kind: u32) -> *mut c_void;
        fn acl_set_fd_np(fd: i32, acl: *mut c_void, kind: u32) -> i32;
        fn acl_free(acl: *mut c_void) -> i32;
        fn acl_init(entries: i32) -> *mut c_void;
    }
    let mut acl = unsafe { acl_get_fd_np(source.as_raw_fd(), 0x100) };
    if acl.is_null() {
        let error = std::io::Error::last_os_error();
        if error.raw_os_error() == Some(45) {
            return Ok(());
        }
        if error.raw_os_error() != Some(2) {
            return Err(error.to_string());
        }
        // Apple's filesec API reports ENOENT when the file has no ACL.
        // Copy an empty ACL to remove any grants inherited by the new file.
        acl = unsafe { acl_init(0) };
        if acl.is_null() {
            return Err(std::io::Error::last_os_error().to_string());
        }
    }
    let result = unsafe { acl_set_fd_np(destination.as_raw_fd(), acl, 0x100) };
    let error = std::io::Error::last_os_error();
    unsafe {
        acl_free(acl);
    }
    if result != 0 {
        return Err(error.to_string());
    }
    Ok(())
}

#[cfg(all(unix, not(any(target_os = "linux", target_os = "macos"))))]
fn copy_acl(_source: &fs::File, _destination: &fs::File) -> Result<(), String> {
    Err(
        "File access-rule preservation is unavailable on this platform; use a new Save As path"
            .into(),
    )
}
