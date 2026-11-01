//! Source-session admission remains alive through every native lease mutation.
//! The ordinary legacy Lease path is deliberately unchanged.
use super::*;
use crate::{
    large_document::Reservation,
    workspace::source_runtime::{SourceDestination, SourceSession},
};
use std::ffi::OsStr;
const HEADERS: usize = 512 * 1024;
const VERSIONS: &str = ".peerbrush-versions";
pub(super) struct Admission {
    versions: fs::File,
    versions_identity: (u64, u64),
    source: SourceDestination,
    _headers: Reservation,
}
fn regular_directory(file: &fs::File) -> Result<(), String> {
    let metadata = file
        .metadata()
        .map_err(|_| "Native directory metadata unavailable")?;
    if !metadata.is_dir() {
        return Err("Native versions owner is not a directory".into());
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if metadata.file_attributes() & (0x400 | 0x4000) != 0 {
            return Err("Native aliased or encrypted versions directory refused".into());
        }
    }
    Ok(())
}
fn versions_open(
    source: &SourceDestination,
    check: &dyn Fn() -> Result<(), String>,
) -> Result<fs::File, String> {
    source.check_before_mutation(check)?;
    #[cfg(unix)]
    {
        use std::os::fd::{AsRawFd, FromRawFd};
        let name = b".peerbrush-versions\0";
        // This is relative to the exact admitted parent; a swapped path is not
        // re-resolved into the private source namespace.
        if unsafe {
            libc::mkdirat(
                source.parent_handle().as_raw_fd(),
                name.as_ptr().cast(),
                0o700,
            )
        } != 0
            && std::io::Error::last_os_error().raw_os_error() != Some(libc::EEXIST)
        {
            return Err("Cannot create native versions storage; names retained".into());
        }
        source.check_before_mutation(check)?;
        let fd = unsafe {
            libc::openat(
                source.parent_handle().as_raw_fd(),
                name.as_ptr().cast(),
                libc::O_RDONLY
                    | libc::O_DIRECTORY
                    | libc::O_NOFOLLOW
                    | libc::O_NONBLOCK
                    | libc::O_CLOEXEC,
            )
        };
        if fd < 0 {
            return Err("Cannot hold native versions storage; names retained".into());
        }
        let file = unsafe { fs::File::from_raw_fd(fd) };
        regular_directory(&file)?;
        Ok(file)
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        let path = source
            .target()
            .parent()
            .ok_or("Native destination parent missing")?
            .join(VERSIONS);
        // The proof retains deny-delete pins for every named ancestor. Do not
        // recanonicalize the selected path or drop those pins around creation.
        match fs::create_dir(&path) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(_) => return Err("Cannot create native versions storage; names retained".into()),
        }
        source.check_before_mutation(check)?;
        let file = fs::OpenOptions::new()
            .read(true)
            .access_mode(0xA0)
            .share_mode(3)
            .custom_flags(0x02000000 | 0x00200000)
            .open(&path)
            .map_err(|_| "Cannot pin native versions storage")?;
        regular_directory(&file)?;
        Ok(file)
    }
}
fn relative_file(directory: &fs::File, path: &Path, name: &OsStr) -> Result<fs::File, String> {
    if !matches!(
        (
            Path::new(name).components().next(),
            Path::new(name).components().nth(1)
        ),
        (Some(std::path::Component::Normal(_)), None)
    ) {
        return Err("Native lock name is not one component".into());
    }
    #[cfg(unix)]
    {
        use std::os::{
            fd::{AsRawFd, FromRawFd},
            unix::ffi::OsStrExt,
        };
        let name =
            std::ffi::CString::new(name.as_bytes()).map_err(|_| "Native lock NUL refused")?;
        let fd = unsafe {
            libc::openat(
                directory.as_raw_fd(),
                name.as_ptr(),
                libc::O_RDWR
                    | libc::O_CREAT
                    | libc::O_NOFOLLOW
                    | libc::O_NONBLOCK
                    | libc::O_CLOEXEC,
                0o600,
            )
        };
        if fd < 0 {
            return Err("Cannot create or hold native lock; existing names retained".into());
        }
        let _ = path;
        Ok(unsafe { fs::File::from_raw_fd(fd) })
    }
    #[cfg(windows)]
    {
        use std::os::windows::{ffi::OsStrExt, fs::OpenOptionsExt};
        if name.encode_wide().any(|u| u == 0 || u == b':' as u16) {
            return Err("Native lock stream alias refused".into());
        }
        let _ = directory;
        fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .share_mode(3)
            .custom_flags(0x00200000)
            .open(path.join(name))
            .map_err(|_| "Cannot create or hold native lock; existing names retained".into())
    }
}
impl NativeLease {
    /// Consumes actual source operation/directory admission. The owner is kept
    /// only through save/receipt completion, never in long-lived FileOwnership.
    pub(crate) fn acquire_admitted(
        source: SourceDestination,
        check: impl Fn() -> Result<(), String>,
    ) -> Result<Self, String> {
        source.check_before_mutation(&check)?;
        let headers = source
            .resident()
            .reserve_bytes(HEADERS, HEADERS)
            .map_err(|e| e.to_string())?;
        let path = source.target().to_owned();
        let versions = versions_open(&source, &check)?;
        let versions_identity = identity(&versions)?;
        let admission = Admission {
            versions,
            versions_identity,
            source,
            _headers: headers,
        };
        let directory = path
            .parent()
            .ok_or("Native destination parent missing")?
            .join(VERSIONS);
        let key = destination_key(&path);
        let name = std::ffi::OsString::from(format!("{:016x}{:016x}.lock", key[0], key[1]));
        admission.check(&check)?;
        let file = relative_file(&admission.versions, &directory, &name)?;
        native_regular(&file).map_err(|_| "Native save lock is not a regular file")?;
        super::native_access::supported(&file)?;
        if file
            .metadata()
            .map_err(|_| "Native lock metadata unavailable")?
            .len()
            != 0
            || link_count(&file)? != 1
        {
            return Err("Native save lock is not an exclusive zero-byte owner".into());
        }
        admission.check(&check)?;
        file.try_lock()
            .map_err(|_| "Native destination save is already busy")?;
        let file = LockedFile(file);
        let expected = snapshot_bounded(&path, FilePolicy::Source36, &check)?;
        admission.check(&check)?;
        Ok(Self(
            Lease {
                _file: file,
                path,
                expected,
                policy: FilePolicy::Source36,
            },
            Some(admission),
        ))
    }
    pub(in crate::file_safety) fn check_native_budget(
        &self,
        budget: &crate::large_document::ResidentBudget,
    ) -> Result<(), String> {
        if self
            .1
            .as_ref()
            .is_some_and(|admission| !admission.source.resident().same_pool(budget))
        {
            return Err("Native inventory belongs to another source resident pool".into());
        }
        Ok(())
    }
    pub(in crate::file_safety) fn check_source_session(
        &self,
        session: &SourceSession,
    ) -> Result<(), String> {
        self.1
            .as_ref()
            .ok_or("Native destination lacks actual source admission")?
            .source
            .check_session(session)
    }
    pub(in crate::file_safety) fn check_before_native_mutation(
        &self,
        check: &dyn Fn() -> Result<(), String>,
    ) -> Result<(), String> {
        check()?;
        if let Some(admission) = &self.1 {
            admission.check(check)?;
        }
        Ok(())
    }
    pub(in crate::file_safety) fn native_parent_handle(&self) -> Result<fs::File, String> {
        if let Some(admission) = &self.1 {
            admission
                .source
                .parent_handle()
                .try_clone()
                .map_err(|_| "Cannot retain admitted native parent".into())
        } else {
            super::owned_rename::open_directory(
                self.path().parent().ok_or("Native parent missing")?,
            )
        }
    }
    pub(in crate::file_safety) fn native_versions_handle(&self) -> Result<fs::File, String> {
        if let Some(admission) = &self.1 {
            admission
                .versions
                .try_clone()
                .map_err(|_| "Cannot retain native versions owner".into())
        } else {
            super::owned_rename::open_directory(
                &self
                    .path()
                    .parent()
                    .ok_or("Native parent missing")?
                    .join(VERSIONS),
            )
        }
    }
    pub(in crate::file_safety) fn open_native_quota(
        &self,
        held: &fs::File,
        check: &dyn Fn() -> Result<(), String>,
    ) -> Result<fs::File, String> {
        self.check_before_native_mutation(check)?;
        let path = self
            .path()
            .parent()
            .ok_or("Native parent missing")?
            .join(VERSIONS);
        if let Some(admission) = &self.1 {
            if identity(held)? != admission.versions_identity {
                return Err("Native quota owner differs from admitted directory".into());
            }
        }
        relative_file(held, &path, OsStr::new("quota.lock"))
    }
}
impl Admission {
    fn check(&self, check: &dyn Fn() -> Result<(), String>) -> Result<(), String> {
        self.source.check_before_mutation(check)?;
        regular_directory(&self.versions)?;
        let parent = self
            .source
            .target()
            .parent()
            .ok_or("Native destination parent missing")?;
        let named = super::owned_rename::open_directory(&parent.join(VERSIONS))?;
        regular_directory(&named)?;
        if identity(&self.versions)? != self.versions_identity
            || identity(&named)? != self.versions_identity
        {
            return Err("Native versions directory changed; admission retained".into());
        }
        Ok(())
    }
}
