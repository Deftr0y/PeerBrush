//! Supported current-file properties. Extra data is refused until its copying
//! and disk accounting are connected; this is not full filesystem qualification.
use crate::large_document::ResidentBudget;
use std::fs::File;
type Check<'a> = &'a dyn Fn() -> Result<(), String>;
#[derive(PartialEq, Eq)]
pub(super) struct Snapshot {
    #[cfg(windows)]
    created: i64,
    #[cfg(windows)]
    attributes: u32,
}
impl Snapshot {
    pub(super) fn capture(
        file: &File,
        budget: &ResidentBudget,
        check: Check<'_>,
    ) -> Result<Self, String> {
        check()?;
        #[cfg(windows)]
        {
            windows::capture(file, budget, check)
        }
        #[cfg(not(windows))]
        {
            no_extra_attributes(file, budget, check)?;
            Ok(Self {})
        }
    }
    pub(super) fn validate_original(
        &self,
        file: &File,
        budget: &ResidentBudget,
        check: Check<'_>,
    ) -> Result<(), String> {
        if Self::capture(file, budget, check)? != *self {
            return Err(
                "Native original filesystem properties changed; all versions retained".into(),
            );
        }
        check()
    }
    pub(super) fn apply(
        &self,
        source: &File,
        current: &File,
        budget: &ResidentBudget,
        check: Check<'_>,
    ) -> Result<(), String> {
        self.validate_original(source, budget, check)?;
        #[cfg(windows)]
        windows::apply(self, current)?;
        self.validate_current(current, budget, check)?;
        self.validate_original(source, budget, check)
    }
    pub(super) fn validate_current(
        &self,
        file: &File,
        budget: &ResidentBudget,
        check: Check<'_>,
    ) -> Result<(), String> {
        #[cfg(windows)]
        {
            let current = Self::capture(file, budget, check)?;
            if current.created != self.created
                || current.attributes != windows::saved_attributes(self.attributes)
            {
                return Err(
                    "Native candidate filesystem properties differ from the original".into(),
                );
            }
        }
        #[cfg(not(windows))]
        no_extra_attributes(file, budget, check)?;
        check()
    }
}
#[cfg(unix)]
fn no_extra_attributes(
    file: &File,
    budget: &ResidentBudget,
    check: Check<'_>,
) -> Result<(), String> {
    use std::os::fd::AsRawFd;
    let mut names = crate::large_document::ScratchBytes::new(budget, 65536, 65536)
        .map_err(|e| e.to_string())?;
    check()?;
    #[cfg(target_os = "linux")]
    let count =
        unsafe { libc::flistxattr(file.as_raw_fd(), names.as_mut_ptr().cast(), names.len()) };
    #[cfg(target_os = "macos")]
    let count =
        unsafe { libc::flistxattr(file.as_raw_fd(), names.as_mut_ptr().cast(), names.len(), 0) };
    if count < 0 {
        let error = std::io::Error::last_os_error();
        if error.raw_os_error() == Some(libc::ENOTSUP) {
            return check();
        }
        return Err("Native extended-attribute enumeration failed; original retained".into());
    }
    let names = &names[..count as usize];
    if names.is_empty() {
        return check();
    }
    if names.last() != Some(&0) {
        return Err("Native extended-attribute names are incomplete".into());
    }
    for name in names[..names.len() - 1].split(|b| *b == 0) {
        #[cfg(target_os = "linux")]
        if name == b"system.posix_acl_access" {
            continue;
        }
        return Err("Native replacement cannot yet preserve extended attributes or resource forks; use a new Save As path".into());
    }
    check()
}

#[cfg(windows)]
mod windows {
    use super::*;
    use ::windows::Win32::Storage::FileSystem::{
        FileBasicInfo, FileStreamInfo, FILE_INFO_BY_HANDLE_CLASS,
    };
    use std::{ffi::c_void, os::windows::io::AsRawHandle, ptr};
    const ALLOWED: u32 = 0x1 | 0x2 | 0x4 | 0x20 | 0x80 | 0x100 | 0x2000;
    const STREAM_BYTES: usize = 65536;
    #[repr(C)]
    #[derive(Default)]
    struct Basic {
        created: i64,
        accessed: i64,
        modified: i64,
        changed: i64,
        attributes: u32,
    }
    #[link(name = "kernel32")]
    extern "system" {
        fn GetFileInformationByHandleEx(
            file: *mut c_void,
            class: FILE_INFO_BY_HANDLE_CLASS,
            buffer: *mut c_void,
            bytes: u32,
        ) -> i32;
        fn SetFileInformationByHandle(
            file: *mut c_void,
            class: FILE_INFO_BY_HANDLE_CLASS,
            buffer: *const c_void,
            bytes: u32,
        ) -> i32;
        fn SetLastError(error: u32);
        fn GetVolumeInformationByHandleW(
            file: *mut c_void,
            name: *mut u16,
            name_len: u32,
            serial: *mut u32,
            component: *mut u32,
            flags: *mut u32,
            filesystem: *mut u16,
            filesystem_len: u32,
        ) -> i32;
    }
    pub(super) fn saved_attributes(attributes: u32) -> u32 {
        (attributes & !0x80) | 0x20
    }
    fn basic(file: &File) -> Result<Basic, String> {
        let mut storage = std::mem::MaybeUninit::<Basic>::zeroed();
        if unsafe {
            GetFileInformationByHandleEx(
                file.as_raw_handle(),
                FileBasicInfo,
                storage.as_mut_ptr().cast(),
                std::mem::size_of::<Basic>() as u32,
            )
        } == 0
        {
            return Err("Native basic file properties unavailable; original retained".into());
        }
        // All fields are integers, and the complete storage was initialized
        // before the synchronous native query, including native ABI padding.
        let basic = unsafe { storage.assume_init() };
        if basic.created <= 0 || basic.attributes & !ALLOWED != 0 {
            return Err("Native replacement cannot preserve these filesystem attributes; use a new Save As path".into());
        }
        Ok(basic)
    }
    pub(super) fn capture(
        file: &File,
        budget: &ResidentBudget,
        check: Check<'_>,
    ) -> Result<Snapshot, String> {
        let before = basic(file)?;
        no_named_streams(file, budget)?;
        check()?;
        let after = basic(file)?;
        if before.created != after.created || before.attributes != after.attributes {
            return Err("Native original filesystem properties changed during capture".into());
        }
        Ok(Snapshot {
            created: after.created,
            attributes: after.attributes,
        })
    }
    pub(super) fn apply(snapshot: &Snapshot, file: &File) -> Result<(), String> {
        // Preserve creation/properties, while a newly saved document keeps its
        // current write/access/change times and is marked for backup (ARCHIVE).
        let mut storage = std::mem::MaybeUninit::<Basic>::zeroed();
        // Write into the final native packet without moving a typed value whose
        // padding could become uninitialized before it crosses the FFI boundary.
        unsafe {
            std::ptr::addr_of_mut!((*storage.as_mut_ptr()).created).write(snapshot.created);
            std::ptr::addr_of_mut!((*storage.as_mut_ptr()).attributes)
                .write(saved_attributes(snapshot.attributes));
        }
        if unsafe {
            SetFileInformationByHandle(
                file.as_raw_handle(),
                FileBasicInfo,
                storage.as_ptr().cast(),
                std::mem::size_of::<Basic>() as u32,
            )
        } == 0
        {
            return Err("Cannot preserve native current-file properties; original retained".into());
        }
        Ok(())
    }
    fn no_named_streams(file: &File, budget: &ResidentBudget) -> Result<(), String> {
        let mut flags = 0;
        if unsafe {
            GetVolumeInformationByHandleW(
                file.as_raw_handle(),
                ptr::null_mut(),
                0,
                ptr::null_mut(),
                ptr::null_mut(),
                &mut flags,
                ptr::null_mut(),
                0,
            )
        } == 0
        {
            return Err("Native volume stream capabilities unavailable; original retained".into());
        }
        if flags & 0x40000 == 0 {
            return Ok(());
        }
        let mut buffer = crate::file_safety::owned_rename::NativeBuffer::new(budget, STREAM_BYTES)?;
        unsafe {
            SetLastError(0);
        }
        let success = unsafe {
            GetFileInformationByHandleEx(
                file.as_raw_handle(),
                FileStreamInfo,
                buffer.as_mut_ptr(),
                STREAM_BYTES as u32,
            )
        };
        let error = std::io::Error::last_os_error().raw_os_error();
        let length = file
            .metadata()
            .map_err(|_| "Native stream length unavailable")?
            .len();
        if error == Some(38)
            && length == 0
            && (success == 0 || buffer.bytes().iter().all(|b| *b == 0))
        {
            return Ok(());
        }
        if success == 0 {
            return Err(
                "Native stream enumeration failed or exceeds admission; original retained".into(),
            );
        }
        main_stream_only(buffer.bytes(), length)
    }
    pub(super) fn main_stream_only(bytes: &[u8], expected: u64) -> Result<(), String> {
        let mut at = 0usize;
        let mut seen = false;
        loop {
            let header = bytes
                .get(at..at.checked_add(24).ok_or("Native stream offset overflow")?)
                .ok_or("Native stream header incomplete")?;
            let next = u32::from_le_bytes(header[..4].try_into().unwrap()) as usize;
            let length = u32::from_le_bytes(header[4..8].try_into().unwrap()) as usize;
            let size = i64::from_le_bytes(header[8..16].try_into().unwrap());
            let allocation = i64::from_le_bytes(header[16..24].try_into().unwrap());
            let end = at
                .checked_add(24)
                .and_then(|n| n.checked_add(length))
                .ok_or("Native stream name overflow")?;
            let name = bytes
                .get(at + 24..end)
                .ok_or("Native stream name incomplete")?;
            if length % 2 != 0
                || size < 0
                || allocation < 0
                || seen
                || size as u64 != expected
                || !name
                    .chunks_exact(2)
                    .map(|b| u16::from_le_bytes([b[0], b[1]]))
                    .eq("::$DATA".encode_utf16())
            {
                return Err(
                    "Native replacement cannot yet preserve named streams; use a new Save As path"
                        .into(),
                );
            }
            seen = true;
            if next == 0 {
                return Ok(());
            }
            if next % 8 != 0 || next < 24 + length {
                return Err("Native stream offsets overlap or are unaligned".into());
            }
            at = at
                .checked_add(next)
                .ok_or("Native stream offset overflow")?;
        }
    }
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;
    use std::{fs, io::Write, path::PathBuf};
    struct Fixture {
        path: PathBuf,
        held: Option<File>,
        identity: (u64, u64),
    }
    impl Fixture {
        fn new() -> Self {
            let path = fs::canonicalize(std::env::temp_dir())
                .unwrap()
                .join(format!("pb-native-metadata-{}", crate::engine::id()));
            fs::create_dir(&path).unwrap();
            let held = crate::file_safety::owned_rename::open_directory(&path).unwrap();
            let identity = crate::file_safety::identity(&held).unwrap();
            Self {
                path,
                held: Some(held),
                identity,
            }
        }
        fn file(&self, name: &str) -> File {
            crate::file_safety::owned_rename::staging_options()
                .open(self.path.join(name))
                .unwrap()
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            if self.path.parent() == Some(fs::canonicalize(std::env::temp_dir()).unwrap().as_path())
                && self
                    .path
                    .file_name()
                    .unwrap()
                    .to_string_lossy()
                    .starts_with("pb-native-metadata-")
                && fs::canonicalize(&self.path).is_ok_and(|p| p == self.path)
                && self
                    .held
                    .as_ref()
                    .is_some_and(|f| crate::file_safety::identity(f).ok() == Some(self.identity))
                && crate::file_safety::owned_rename::open_directory(&self.path)
                    .is_ok_and(|f| crate::file_safety::identity(&f).ok() == Some(self.identity))
            {
                drop(self.held.take());
                fs::remove_dir_all(&self.path).unwrap();
            }
        }
    }
    fn stream(name: &str, size: i64) -> Vec<u8> {
        let name = name
            .encode_utf16()
            .flat_map(u16::to_le_bytes)
            .collect::<Vec<_>>();
        let mut result = vec![0; (24 + name.len() + 7) & !7];
        result[4..8].copy_from_slice(&(name.len() as u32).to_le_bytes());
        result[8..16].copy_from_slice(&size.to_le_bytes());
        result[16..24].copy_from_slice(&size.to_le_bytes());
        result[24..24 + name.len()].copy_from_slice(&name);
        result
    }
    #[test]
    fn stream_parser_rejects_forks_duplicate_main_streams_and_malformed_offsets() {
        let main = stream("::$DATA", 17);
        windows::main_stream_only(&main, 17).unwrap();
        for n in [0, 1, 23, 37] {
            assert!(windows::main_stream_only(&main[..n], 17).is_err());
        }
        assert!(windows::main_stream_only(&main, 18).is_err());
        assert!(windows::main_stream_only(&stream("::$DATA", -1), 17).is_err());
        assert!(windows::main_stream_only(&stream(":notes:$DATA", 17), 17).is_err());
        for second in [stream(":notes:$DATA", 0), stream("::$DATA", 17)] {
            let mut bytes = main.clone();
            bytes[..4].copy_from_slice(&(main.len() as u32).to_le_bytes());
            bytes.extend(second);
            assert!(windows::main_stream_only(&bytes, 17).is_err());
        }
        for offset in [1u32, 8, 32, u32::MAX] {
            let mut bytes = main.clone();
            bytes[..4].copy_from_slice(&offset.to_le_bytes());
            assert!(windows::main_stream_only(&bytes, 17).is_err());
        }
    }
    #[test]
    fn current_file_retains_creation_and_attributes_and_detects_later_property_changes() {
        let f = Fixture::new();
        let pool = ResidentBudget::new(512 * 1024).unwrap();
        let mut source = f.file("original.psb");
        source.write_all(b"old data").unwrap();
        source.sync_all().unwrap();
        let before = Snapshot::capture(&source, &pool, &|| Ok(())).unwrap();
        let custom = Snapshot {
            created: before.created - 600_000_000,
            attributes: before.attributes | 2 | 0x2000,
        };
        windows::apply(&custom, &source).unwrap();
        let original = Snapshot::capture(&source, &pool, &|| Ok(())).unwrap();
        let current = f.file("candidate.tmp");
        let stamp = current.metadata().unwrap().modified().unwrap();
        original
            .apply(&source, &current, &pool, &|| Ok(()))
            .unwrap();
        original
            .validate_current(&current, &pool, &|| Ok(()))
            .unwrap();
        assert_eq!(current.metadata().unwrap().modified().unwrap(), stamp);
        assert_eq!(source.metadata().unwrap().len(), 8);
        let changed = Snapshot {
            created: original.created,
            attributes: original.attributes ^ 2,
        };
        windows::apply(&changed, &source).unwrap();
        assert!(original
            .validate_original(&source, &pool, &|| Ok(()))
            .is_err());
        windows::apply(&changed, &current).unwrap();
        assert!(original
            .validate_current(&current, &pool, &|| Ok(()))
            .is_err());
        assert_eq!(pool.used(), 0);
    }
    #[test]
    fn real_named_stream_refusal_keeps_its_main_data_and_metadata_untouched() {
        let f = Fixture::new();
        let path = f.path.join("original.psb");
        fs::write(&path, b"old main data").unwrap();
        let stream = path.with_file_name("original.psb:notes");
        fs::write(&stream, b"keep this metadata").unwrap();
        let source = crate::file_safety::open_native_read(&path).unwrap();
        let pool = ResidentBudget::new(512 * 1024).unwrap();
        assert!(Snapshot::capture(&source, &pool, &|| Ok(())).is_err());
        assert_eq!(fs::read(&path).unwrap(), b"old main data");
        assert_eq!(fs::read(&stream).unwrap(), b"keep this metadata");
        assert_eq!(pool.used(), 0);
    }
}
