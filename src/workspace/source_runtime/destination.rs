//! Live destination admission before a versions directory or lock is created.
use super::{Directory, RootOperation, SourceRuntime, SourceSession};
use crate::large_document::Reservation;
use std::{
    ffi::OsStr,
    fs::{self, File},
    path::{Path, PathBuf},
    sync::{atomic::Ordering, Arc},
};
const MAX_PATH: usize = 64 * 1024;
const MAX_ANCESTORS: usize = 64;

/// Holds actual parent/namespace observations and operation lifetime, rather
/// than promoting a canonical string or a copied FileId into write authority.
/// The native lease consumes it and uses its held parent for relative creation.
#[must_use]
pub(crate) struct SourceDestination {
    requested: PathBuf,
    target: PathBuf,
    ancestors: Vec<Directory>,
    #[cfg(windows)]
    pins: Vec<File>,
    session: [u8; 16],
    runtime: Arc<SourceRuntime>,
    // Keep payloads/handles before their admission, and actual operation last.
    _paths: Reservation,
    _headers: Reservation,
    operation: Arc<RootOperation>,
}
impl SourceSession {
    pub(crate) fn admit_destination(
        &self,
        path: &Path,
        check: &dyn Fn() -> Result<(), String>,
    ) -> Result<SourceDestination, String> {
        check()?;
        self.check()?;
        if path.as_os_str().len() > MAX_PATH {
            return Err("Native destination path exceeds bounded admission".into());
        }
        #[cfg(windows)]
        {
            use std::os::windows::ffi::OsStrExt;
            if path.file_name().is_none_or(|name| {
                name.encode_wide()
                    .any(|unit| unit == 0 || unit == b':' as u16)
            }) {
                return Err("Native destination stream aliases are refused".into());
            }
        }
        let paths = self
            .resident()
            .reserve_bytes(4 * MAX_PATH, 4 * MAX_PATH)
            .map_err(|e| e.to_string())?;
        let target = crate::file_safety::canonical_destination(path)?;
        if target.as_os_str().len() > MAX_PATH || target.file_name().is_none() {
            return Err("Native destination needs a bounded file name".into());
        }
        if managed(target.file_name()) {
            return Err("Managed previous-save storage is not a user destination".into());
        }
        let parent = target.parent().ok_or("Native destination has no parent")?;
        let count = parent.ancestors().count();
        if count == 0 || count > MAX_ANCESTORS {
            return Err("Native destination ancestor admission exceeded".into());
        }
        let headers = parent.ancestors().try_fold(4096usize, |total, p| {
            total
                .checked_add(
                    p.as_os_str()
                        .len()
                        .checked_mul(4)
                        .ok_or("Destination ancestor footprint overflow")?,
                )
                .and_then(|n| {
                    n.checked_add(
                        2 * std::mem::size_of::<Directory>() + 2 * std::mem::size_of::<File>(),
                    )
                })
                .ok_or("Destination ancestor footprint overflow")
        })?;
        let admission = self
            .resident()
            .reserve_bytes(headers, 64 * 1024 * 1024)
            .map_err(|e| e.to_string())?;
        let mut ancestors = Vec::new();
        ancestors
            .try_reserve_exact(count)
            .map_err(|_| "Destination ancestor allocation refused")?;
        #[cfg(windows)]
        let mut pins = {
            let mut files = Vec::new();
            files
                .try_reserve_exact(count)
                .map_err(|_| "Destination name pin allocation refused")?;
            files
        };
        for path in parent.ancestors() {
            check()?;
            self.check()?;
            if managed(path.file_name()) {
                return Err("Managed previous-save storage is not a user destination".into());
            }
            let directory = Directory::open(path)?;
            #[cfg(windows)]
            {
                use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
                let metadata = fs::symlink_metadata(path)
                    .map_err(|_| "Cannot inspect destination ancestor")?;
                if metadata.file_attributes() & 0x400 != 0 {
                    return Err("Destination ancestor alias refused".into());
                }
                let pin = fs::OpenOptions::new()
                    .read(true)
                    .access_mode(0xA0)
                    .share_mode(3)
                    .custom_flags(0x02000000 | 0x00200000)
                    .open(path)
                    .map_err(|_| "Cannot pin destination ancestor name")?;
                if crate::file_safety::identity(&pin)? != directory.identity() {
                    return Err("Destination ancestor changed while pinning".into());
                }
                pins.push(pin);
            }
            ancestors.push(directory);
        }
        let proof = SourceDestination {
            requested: path.to_owned(),
            target,
            ancestors,
            #[cfg(windows)]
            pins,
            session: self.id,
            runtime: self.runtime.clone(),
            _paths: paths,
            _headers: admission,
            operation: self.operation.clone(),
        };
        proof.check_before_mutation(check)?;
        Ok(proof)
    }
}
impl SourceDestination {
    pub(crate) fn target(&self) -> &Path {
        &self.target
    }
    pub(crate) fn file_name(&self) -> &OsStr {
        self.target
            .file_name()
            .expect("admitted destination file name")
    }
    pub(crate) fn parent_handle(&self) -> &File {
        self.ancestors[0].held()
    }
    pub(crate) fn parent_identity(&self) -> (u64, u64) {
        self.ancestors[0].identity()
    }
    pub(crate) fn resident(&self) -> crate::large_document::ResidentBudget {
        self.runtime.resident()
    }
    pub(crate) fn check_session(&self, session: &SourceSession) -> Result<(), String> {
        if self.session != session.id
            || !Arc::ptr_eq(&self.runtime, &session.runtime)
            || !Arc::ptr_eq(&self.operation, &session.operation)
        {
            return Err("Destination belongs to another source operation".into());
        }
        self.check_before_mutation(&|| Ok(()))
    }
    /// The consumer must create/open relative to parent_handle(), not resolve
    /// target() again. Windows ancestor name pins remain alive with this proof.
    pub(crate) fn check_before_mutation(
        &self,
        check: &dyn Fn() -> Result<(), String>,
    ) -> Result<(), String> {
        check()?;
        self.operation.check()?;
        if self.runtime.tainted.load(Ordering::Acquire) {
            return Err("Source namespace admission is latched; destination refused".into());
        }
        if crate::file_safety::canonical_destination(&self.requested)? != self.target {
            return Err("Destination name changed before native admission".into());
        }
        let namespace = {
            let state = self
                .runtime
                .inventory
                .try_lock()
                .map_err(|_| "Source inventory is busy")?;
            let inventory = state.as_ref().ok_or("Source inventory unavailable")?;
            inventory.directory.check()?;
            for held in &inventory.files {
                held.check()?;
            }
            inventory.directory.identity()
        };
        for directory in &self.ancestors {
            directory.check()?;
            if directory.identity() == namespace {
                return Err("Native source storage cannot be a user destination".into());
            }
        }
        self.check_current_ancestry(namespace)?;
        let parent = self.ancestors[0].identity();
        let target_identity = match fs::symlink_metadata(&self.target) {
            Ok(metadata) => {
                if !metadata.is_file() || metadata.file_type().is_symlink() {
                    return Err("Native destination is not a regular file".into());
                }
                let file = crate::file_safety::open_native_identity(&self.target)
                    .map_err(|_| "Native destination identity unavailable")?;
                if crate::file_safety::link_count(&file)? != 1 {
                    return Err("Native destination file alias refused".into());
                }
                Some(crate::file_safety::identity(&file)?)
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(_) => return Err("Cannot inspect native destination".into()),
        };
        if let Some(id) = target_identity {
            if self
                .runtime
                .disk
                .is_internal_identity(id)
                .map_err(|e| e.to_string())?
            {
                return Err("Native backing or staging cannot be an external destination".into());
            }
        }
        let state = self
            .runtime
            .inventory
            .try_lock()
            .map_err(|_| "Source inventory is busy")?;
        let inventory = state.as_ref().ok_or("Source inventory unavailable")?;
        inventory
            .publications
            .refuse_destination(parent, self.file_name(), target_identity)?;
        drop(state);
        // No caller callback follows the final complete named-ancestor pass.
        for directory in &self.ancestors {
            directory.check()?;
        }
        self.operation.check()
    }
    #[cfg(unix)]
    fn check_current_ancestry(&self, namespace: (u64, u64)) -> Result<(), String> {
        use std::os::fd::{AsRawFd, FromRawFd};
        let mut current = self
            .parent_handle()
            .try_clone()
            .map_err(|_| "Cannot retain current destination ancestry")?;
        for _ in 0..MAX_ANCESTORS {
            let id = crate::file_safety::identity(&current)?;
            if id == namespace {
                return Err("Current destination ancestry enters native source storage".into());
            }
            // The constant NUL-terminated name is relative to our held directory.
            // O_DIRECTORY/NOFOLLOW refuse non-directory aliases; successful fd
            // ownership transfers exactly once into File, including error paths.
            let fd = unsafe {
                libc::openat(
                    current.as_raw_fd(),
                    b"..\0".as_ptr().cast(),
                    libc::O_RDONLY
                        | libc::O_DIRECTORY
                        | libc::O_NOFOLLOW
                        | libc::O_CLOEXEC
                        | libc::O_NONBLOCK,
                )
            };
            if fd < 0 {
                return Err("Cannot observe current destination parent".into());
            }
            let parent = unsafe { File::from_raw_fd(fd) };
            if crate::file_safety::identity(&parent)? == id {
                return Ok(());
            }
            current = parent;
        }
        Err("Current destination ancestry exceeds bounded admission".into())
    }
    #[cfg(windows)]
    fn check_current_ancestry(&self, namespace: (u64, u64)) -> Result<(), String> {
        use std::{
            ffi::{c_void, OsString},
            os::windows::{ffi::OsStringExt, io::AsRawHandle},
        };
        #[link(name = "kernel32")]
        unsafe extern "system" {
            fn GetFinalPathNameByHandleW(
                file: *mut c_void,
                path: *mut u16,
                count: u32,
                flags: u32,
            ) -> u32;
        }
        // API: https://learn.microsoft.com/en-us/windows/win32/api/fileapi/nf-fileapi-getfinalpathnamebyhandlew
        let mut path = Vec::new();
        path.try_reserve_exact(MAX_PATH / 2)
            .map_err(|_| "Current destination name allocation refused")?;
        path.resize(MAX_PATH / 2, 0);
        let count = unsafe {
            GetFinalPathNameByHandleW(
                self.parent_handle().as_raw_handle(),
                path.as_mut_ptr(),
                path.len() as u32,
                0,
            )
        } as usize;
        if count == 0 || count >= path.len() {
            return Err("Current destination handle name is unavailable or unbounded".into());
        }
        let current = PathBuf::from(OsString::from_wide(&path[..count]));
        let mut count = 0;
        for path in current.ancestors() {
            count += 1;
            if count > MAX_ANCESTORS {
                return Err("Current destination ancestry exceeds admission".into());
            }
            let directory = Directory::open(path)?;
            if directory.identity() == namespace {
                return Err("Current destination ancestry enters native source storage".into());
            }
            if count == 1 && directory.identity() != self.parent_identity() {
                return Err("Current destination handle name changed identity".into());
            }
        }
        Ok(())
    }
}
fn managed(name: Option<&OsStr>) -> bool {
    #[cfg(windows)]
    {
        name.and_then(OsStr::to_str)
            .is_some_and(|n| n.eq_ignore_ascii_case(".peerbrush-versions"))
    }
    #[cfg(unix)]
    {
        name == Some(OsStr::new(".peerbrush-versions"))
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::{limits, Fixture};
    use super::*;
    use crate::{file_safety::NativeLease, large_document::root::PersistentRootContext};
    #[test]
    fn live_outside_destination_has_exact_session_parent_and_real_guarded_lease() {
        let f = Fixture::new();
        let (_engine, connection) = f.connection();
        let context = PersistentRootContext::from_explicit_state(&connection).unwrap();
        let runtime = SourceRuntime::new(context, limits()).unwrap();
        let session = runtime.begin().unwrap();
        let base = connection.state_dir.parent().unwrap();
        let path = base.join("native-output.psb");
        let proof = session.admit_destination(&path, &|| Ok(())).unwrap();
        assert_eq!(proof.target(), path);
        assert_eq!(
            proof.parent_identity(),
            crate::file_safety::identity(proof.parent_handle()).unwrap()
        );
        let other = runtime.begin().unwrap();
        assert!(proof.check_session(&other).is_err());
        drop(other);
        assert!(!base.join(".peerbrush-versions").exists());
        let lease = NativeLease::acquire_admitted(proof, &|| Ok(())).unwrap();
        assert_eq!(lease.path(), path);
        assert!(base.join(".peerbrush-versions").is_dir());
        drop(lease);
        drop(session);
        drop(runtime);
        drop(connection);
    }
    #[test]
    fn namespace_and_descendants_refuse_before_any_versions_creation() {
        let f = Fixture::new();
        let (_engine, connection) = f.connection();
        let context = PersistentRootContext::from_explicit_state(&connection).unwrap();
        let runtime = SourceRuntime::new(context, limits()).unwrap();
        let session = runtime.begin().unwrap();
        let source = session.store_for_lineage([4; 16]).unwrap();
        let child = runtime
            .inventory
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .locations
            .get(&source.identity())
            .unwrap()
            .path
            .clone();
        for parent in [session.directory.clone(), child] {
            assert!(session
                .admit_destination(&parent.join("output.psb"), &|| Ok(()))
                .is_err());
            assert!(!parent.join(".peerbrush-versions").exists());
        }
        #[cfg(windows)]
        assert!(session
            .admit_destination(
                &connection
                    .state_dir
                    .parent()
                    .unwrap()
                    .join("output.psb:stream"),
                &|| Ok(())
            )
            .is_err());
        let managed = connection
            .state_dir
            .parent()
            .unwrap()
            .join(".peerbrush-versions");
        assert!(session.admit_destination(&managed, &|| Ok(())).is_err());
        assert!(!managed.exists());
        fs::create_dir(&managed).unwrap();
        assert!(session
            .admit_destination(&managed.join("history.psb"), &|| Ok(()))
            .is_err());
        assert!(!managed.join(".peerbrush-versions").exists());
        drop(source);
        drop(session);
        drop(runtime);
        drop(connection);
    }
    #[test]
    fn current_internal_file_id_is_rechecked_before_guarded_lease_can_create_versions() {
        let f = Fixture::new();
        let (_engine, connection) = f.connection();
        let context = PersistentRootContext::from_explicit_state(&connection).unwrap();
        let runtime = SourceRuntime::new(context, limits()).unwrap();
        let session = runtime.begin().unwrap();
        let base = connection.state_dir.parent().unwrap();
        let path = base.join("aliased.psb");
        let proof = session.admit_destination(&path, &|| Ok(())).unwrap();
        let temporary = session
            .byte_temporary(1, crate::large_document::ByteRole::LayerData)
            .unwrap();
        let original = runtime
            .inventory
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .staging
            .values()
            .next()
            .unwrap()
            .path
            .join("bytes.pbb");
        fs::rename(&original, &path).unwrap();
        assert!(proof.check_before_mutation(&|| Ok(())).is_err());
        assert!(NativeLease::acquire_admitted(proof, &|| Ok(())).is_err());
        assert!(!base.join(".peerbrush-versions").exists());
        fs::rename(&path, &original).unwrap();
        temporary.abort(&mut || Ok(())).unwrap();
        drop(session);
        drop(runtime);
        drop(connection);
    }
}
