//! No-clobber publication of a verified held staging file, not path authority.
use crate::large_document::{
    verified_transfer_buffer, Reservation, ResidentBudget, ScratchBytes, VERIFIED_BATCH_BYTES,
};
use ring::digest::{Context, SHA256};
use std::{
    fs::{self, File, OpenOptions},
    path::{Component, Path},
    time::SystemTime,
};
type Check<'a> = &'a mut dyn FnMut() -> Result<(), String>;
const MAXIMUM: u64 = 64 * 1024 * 1024 * 1024;
const CHUNK: usize = 64 * 1024;

pub(crate) struct Sealed {
    identity: (u64, u64),
    bytes: u64,
    modified: SystemTime,
    budget: ResidentBudget,
}
impl Sealed {
    /// The caller retains source/profile admission and its exclusive write owner.
    /// Verification and aligned native buffers use this operation's actual pool.
    pub(crate) fn verify(
        file: &File,
        bytes: u64,
        expected: [u8; 32],
        budget: &ResidentBudget,
        check: Check<'_>,
    ) -> Result<Self, String> {
        if bytes > MAXIMUM {
            return Err("Owned publication bounds exceeded".into());
        }
        let mut scratch = verified_transfer_buffer(budget, bytes).map_err(|e| e.to_string())?;
        Self::verify_with_buffer(file, bytes, expected, budget, &mut scratch, check)
    }

    pub(in crate::file_safety) fn verify_with_buffer(
        file: &File,
        bytes: u64,
        expected: [u8; 32],
        budget: &ResidentBudget,
        scratch: &mut ScratchBytes,
        check: Check<'_>,
    ) -> Result<Self, String> {
        if bytes > MAXIMUM {
            return Err("Owned publication bounds exceeded".into());
        }
        if scratch.is_empty()
            || scratch.len() > VERIFIED_BATCH_BYTES
            || !scratch.uses_budget(budget)
        {
            return Err("Owned publication verification buffer differs from its admission".into());
        }
        check()?;
        file.sync_all()
            .map_err(|_| "Owned staging sync failed; files retained")?;
        let metadata = file
            .metadata()
            .map_err(|_| "Owned staging file unavailable")?;
        if !metadata.is_file() || metadata.len() != bytes || super::link_count(file)? != 1 {
            return Err("Owned staging shape changed".into());
        }
        let sealed = Self {
            identity: super::identity(file)?,
            bytes,
            modified: metadata
                .modified()
                .map_err(|_| "Owned staging stamp unavailable")?,
            budget: budget.clone(),
        };
        let mut context = Context::new(&SHA256);
        let mut at = 0;
        while at < bytes {
            check()?;
            let count = (bytes - at).min(scratch.len() as u64) as usize;
            read_at(file, at, &mut scratch[..count])?;
            context.update(&scratch[..count]);
            at += count as u64;
        }
        if context.finish().as_ref() != expected {
            return Err("Owned staging bytes differ from their expected SHA256".into());
        }
        check()?;
        sealed.check(file)?;
        Ok(sealed)
    }
    pub(in crate::file_safety) fn check(&self, file: &File) -> Result<(), String> {
        let metadata = file
            .metadata()
            .map_err(|_| "Owned staging file unavailable")?;
        if super::identity(file)? != self.identity
            || !metadata.is_file()
            || metadata.len() != self.bytes
            || metadata.modified().ok() != Some(self.modified)
            || super::link_count(file)? != 1
        {
            return Err("Owned staging bytes or identity changed; files retained".into());
        }
        Ok(())
    }
    pub(crate) fn check_path(&self, file: &File, path: &Path) -> Result<(), String> {
        self.check(file)?;
        named(file, path, self.identity)
    }
}

pub(crate) fn staging_options() -> OpenOptions {
    let mut options = OpenOptions::new();
    options.read(true).write(true).create_new(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options
            .access_mode(0xC0010000)
            .share_mode(5)
            .custom_flags(0x00200000);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    options
}

/// Only for newly created owned outputs, never the original source. Windows
/// otherwise defers automatic write stamps until the writing handle closes.
pub(in crate::file_safety) fn hold_write_stamp(file: &File) -> Result<(), String> {
    #[cfg(windows)]
    {
        use std::{os::windows::io::AsRawHandle, ptr};
        #[repr(C)]
        struct FileTime {
            low: u32,
            high: u32,
        }
        #[link(name = "kernel32")]
        extern "system" {
            fn SetFileTime(
                file: *mut std::ffi::c_void,
                created: *const FileTime,
                accessed: *const FileTime,
                modified: *const FileTime,
            ) -> i32;
        }
        let unchanged = FileTime {
            low: u32::MAX,
            high: u32::MAX,
        };
        // The documented sentinel suppresses subsequent automatic updates on
        // this handle without changing the stored time. It requires WRITE_ATTRIBUTES.
        // https://learn.microsoft.com/en-us/windows/win32/api/fileapi/nf-fileapi-setfiletime
        if unsafe { SetFileTime(file.as_raw_handle(), ptr::null(), ptr::null(), &unchanged) } == 0 {
            return Err("Cannot stabilize owned output timestamp; file and debt retained".into());
        }
    }
    let _ = file;
    Ok(())
}
pub(crate) fn finish_write_stamp(file: &File) -> Result<(), String> {
    #[cfg(windows)]
    {
        file.set_times(fs::FileTimes::new().set_modified(SystemTime::now()))
            .map_err(|_| "Cannot finalize owned output timestamp; file and debt retained")?;
        hold_write_stamp(file)?;
    }
    let _ = file;
    Ok(())
}

fn component(path: &Path) -> Result<&std::ffi::OsStr, String> {
    match (path.components().next(), path.components().nth(1)) {
        (Some(Component::Normal(name)), None) => {
            #[cfg(windows)]
            {
                use std::os::windows::ffi::OsStrExt;
                if name
                    .encode_wide()
                    .any(|unit| unit == 0 || unit == b':' as u16)
                {
                    return Err("Owned publication refuses stream or NUL names".into());
                }
            }
            Ok(name)
        }
        _ => Err("Owned publication requires a single internal file name".into()),
    }
}
fn named(file: &File, path: &Path, identity: (u64, u64)) -> Result<(), String> {
    let metadata = fs::symlink_metadata(path).map_err(|e| {
        format!(
            "Owned staging metadata unavailable ({:?})",
            e.raw_os_error()
        )
    })?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err("Owned staging name changed".into());
    }
    let opened = super::open_native_identity(path).map_err(|e| {
        format!(
            "Owned staging identity unavailable ({:?})",
            e.raw_os_error()
        )
    })?;
    if super::identity(&opened)? != identity
        || super::identity(file)? != identity
        || super::link_count(&opened)? != 1
    {
        return Err("Owned staging name changed; replacement retained".into());
    }
    Ok(())
}

/// Atomic destination refusal. Callers supply an actually admitted same-directory
/// owner, not a deserialized path. On any failure/uncertainty do not delete names
/// or refund quota. POSIX source-name races remain path based; no fd-rename claim.
pub(crate) fn publish_new(
    directory: &Path,
    held_directory: &File,
    from: &Path,
    to: &Path,
    file: &File,
    sealed: &Sealed,
    check: Check<'_>,
) -> Result<(), String> {
    relocate_new(
        directory,
        held_directory,
        directory,
        held_directory,
        from,
        to,
        file,
        sealed,
        check,
    )
}

/// Move the physical owner between admitted directories without replacing any
/// destination. No copy, unlink, cross-volume fallback or quota refund occurs.
/// POSIX source-name races still require the named postcheck; an uncertain move
/// retains all files and cannot grant a publication or recovery receipt.
#[allow(clippy::too_many_arguments)]
pub(in crate::file_safety) fn relocate_new(
    from_directory: &Path,
    held_from: &File,
    to_directory: &Path,
    held_to: &File,
    from: &Path,
    to: &Path,
    file: &File,
    sealed: &Sealed,
    check: Check<'_>,
) -> Result<(), String> {
    let from_name = component(from)?;
    let to_name = component(to)?;
    let from_id = super::identity(held_from)?;
    let to_id = super::identity(held_to)?;
    if (from_id == to_id && from_name == to_name)
        || !from_directory.is_absolute()
        || !to_directory.is_absolute()
    {
        return Err("Owned publication requires distinct internal names".into());
    }
    if from_id.0 != to_id.0 || from_id.0 != sealed.identity.0 {
        return Err("Owned relocation requires one admitted volume; files retained".into());
    }
    let observe = || -> Result<(), String> {
        for (path, held, expected) in [
            (from_directory, held_from, from_id),
            (to_directory, held_to, to_id),
        ] {
            let metadata = fs::symlink_metadata(path).map_err(|_| "Owned directory unavailable")?;
            if !metadata.is_dir() || metadata.file_type().is_symlink() {
                return Err("Owned directory changed".into());
            }
            #[cfg(windows)]
            {
                use std::os::windows::fs::MetadataExt;
                if metadata.file_attributes() & 0x400 != 0 {
                    return Err("Owned directory alias refused".into());
                }
            }
            let opened = open_directory(path)?;
            if super::identity(&opened)? != expected || super::identity(held)? != expected {
                return Err("Owned directory identity changed".into());
            }
        }
        Ok(())
    };
    check()?;
    observe()?;
    sealed.check(file)?;
    named(file, &from_directory.join(from_name), sealed.identity)?;
    #[cfg(windows)]
    let _from_pins = pin_directories(from_directory, from_id)?;
    #[cfg(windows)]
    let _to_pins = if from_id == to_id {
        None
    } else {
        Some(pin_directories(to_directory, to_id)?)
    };
    // Last application source/revision/lease check is after all payload sync/readback.
    check()?;
    observe()?;
    sealed.check(file)?;
    named(file, &from_directory.join(from_name), sealed.identity)?;
    rename_new(
        held_from,
        held_to,
        to_directory,
        from_name,
        to_name,
        file,
        &sealed.budget,
    )?;
    file.sync_all()
        .map_err(|_| "Owned publication sync uncertain; files retained")?;
    crate::server::sync_parent(&to_directory.join(to_name))?;
    if from_id != to_id {
        crate::server::sync_parent(&from_directory.join(from_name))?;
    }
    observe()?;
    sealed.check(file)?;
    named(file, &to_directory.join(to_name), sealed.identity)?;
    if !matches!(fs::symlink_metadata(from_directory.join(from_name)), Err(error) if error.kind() == std::io::ErrorKind::NotFound)
    {
        return Err("Owned publication source outcome uncertain; names retained".into());
    }
    Ok(())
}

pub(in crate::file_safety) fn open_directory(path: &Path) -> Result<File, String> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options
            .access_mode(0xA0)
            .share_mode(7)
            .custom_flags(0x02000000 | 0x00200000);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW);
    }
    options
        .open(path)
        .map_err(|_| "Owned directory handle unavailable".into())
}
#[cfg(windows)]
pub(in crate::file_safety) fn pin_directories(
    path: &Path,
    expected: (u64, u64),
) -> Result<Vec<File>, String> {
    use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
    let mut pins = Vec::new();
    pins.try_reserve_exact(64)
        .map_err(|_| "Owned directory pin allocation refused")?;
    for ancestor in path.ancestors() {
        if pins.len() == 64 {
            return Err("Owned directory depth exceeds publication admission".into());
        }
        let metadata = fs::symlink_metadata(ancestor).map_err(|_| "Owned ancestor unavailable")?;
        if !metadata.is_dir() || metadata.file_attributes() & 0x400 != 0 {
            return Err("Owned ancestor alias refused".into());
        }
        let file = OpenOptions::new()
            .read(true)
            .access_mode(0xA0)
            .share_mode(3)
            .custom_flags(0x02000000 | 0x00200000)
            .open(ancestor)
            .map_err(|_| "Cannot retain exclusive directory-name admission")?;
        let named = open_directory(ancestor)?;
        if super::identity(&file)? != super::identity(&named)? {
            return Err("Owned ancestor identity changed".into());
        }
        pins.push(file);
    }
    if pins.first().map(super::identity).transpose()? != Some(expected) {
        return Err("Owned directory identity changed".into());
    }
    Ok(pins)
}

#[cfg(windows)]
pub(in crate::file_safety) struct NativeBuffer {
    words: Box<[u64]>,
    _charge: Reservation,
}
#[cfg(windows)]
impl NativeBuffer {
    pub(in crate::file_safety) fn new(
        budget: &ResidentBudget,
        bytes: usize,
    ) -> Result<Self, String> {
        if bytes == 0 {
            return Err("Native buffer must not be empty".into());
        }
        let count = bytes
            .checked_add(7)
            .ok_or("Native rename buffer overflow")?
            / 8;
        let length = count
            .checked_mul(8)
            .ok_or("Native rename buffer overflow")?;
        let charge = budget
            .reserve_bytes(length, 2 * CHUNK)
            .map_err(|e| e.to_string())?;
        let layout =
            std::alloc::Layout::array::<u64>(count).map_err(|_| "Native rename layout invalid")?;
        // SAFETY: count is nonzero, bounded and charged before allocation. Every
        // u64 is initialized to zero. The uniquely owned Box uses the exact same
        // global allocator, length and aligned layout when it is deallocated.
        let pointer = std::ptr::NonNull::new(unsafe { std::alloc::alloc_zeroed(layout) })
            .ok_or("Native rename allocation refused")?;
        let words = unsafe {
            Box::from_raw(std::ptr::slice_from_raw_parts_mut(
                pointer.as_ptr().cast::<u64>(),
                count,
            ))
        };
        Ok(Self {
            words,
            _charge: charge,
        })
    }
    pub(in crate::file_safety) fn as_mut_ptr(&mut self) -> *mut std::ffi::c_void {
        self.words.as_mut_ptr().cast()
    }
    pub(in crate::file_safety) fn bytes(&self) -> &[u8] {
        // The initialized aligned allocation outlives the returned shared view.
        unsafe { std::slice::from_raw_parts(self.words.as_ptr().cast(), self.words.len() * 8) }
    }
}

#[cfg(windows)]
fn rename_new(
    _source_directory: &File,
    directory: &File,
    path: &Path,
    _from: &std::ffi::OsStr,
    to: &std::ffi::OsStr,
    file: &File,
    budget: &ResidentBudget,
) -> Result<(), String> {
    use ::windows::Win32::Storage::FileSystem::{FileRenameInfo, FILE_INFO_BY_HANDLE_CLASS};
    use std::os::windows::{ffi::OsStrExt, io::AsRawHandle};
    #[link(name = "kernel32")]
    extern "system" {
        fn SetFileInformationByHandle(
            file: *mut std::ffi::c_void,
            kind: FILE_INFO_BY_HANDLE_CLASS,
            data: *const std::ffi::c_void,
            bytes: u32,
        ) -> i32;
    }
    let target_directory = open_directory(path)?;
    if super::identity(&target_directory)? != super::identity(directory)? {
        return Err("Owned rename directory changed; files retained".into());
    }
    #[repr(C)]
    struct RenameInfo {
        flags: u32,
        root: *mut std::ffi::c_void,
        name_bytes: u32,
        name: [u16; 1],
    }
    let length_at = std::mem::offset_of!(RenameInfo, name_bytes);
    let name_at = std::mem::offset_of!(RenameInfo, name);
    let target = fs::canonicalize(path)
        .map_err(|_| "Pinned destination path unavailable")?
        .join(to);
    let count = target.as_os_str().encode_wide().count();
    if count == 0 || count > 32767 {
        return Err("Owned destination exceeds its native path bound".into());
    }
    // FileName's documented representation is NUL-terminated even though its
    // counted length excludes that terminator. Do not rely on allocator padding.
    let packet_bytes = name_at + (count + 1) * 2;
    let mut storage = NativeBuffer::new(budget, packet_bytes)?;
    // Documented absolute extended path + NULL RootDirectory. Directory and ALL
    // ancestors remain pinned without FILE_SHARE_DELETE through final postchecks,
    // preventing namespace replacement during the Win32 path resolution.
    // Actual native tests reject the relative non-null RootDirectory wrapper.
    // https://learn.microsoft.com/en-us/windows/win32/api/winbase/ns-winbase-file_rename_info
    for (index, unit) in target.as_os_str().encode_wide().enumerate() {
        if unit == 0 {
            return Err("Owned destination name exceeds its native bound".into());
        }
        // SAFETY: storage is aligned, initialized, exclusively owned and large
        // enough for the checked WCHAR offset. No reference aliases these bytes.
        unsafe {
            storage
                .words
                .as_mut_ptr()
                .cast::<u8>()
                .add(name_at + index * 2)
                .cast::<u16>()
                .write_unaligned(unit);
        }
    }
    // SAFETY: offset_of names a u32 field inside the initialized native header;
    // its checked name payload also fits this exclusively owned aligned buffer.
    unsafe {
        storage
            .words
            .as_mut_ptr()
            .cast::<u8>()
            .add(length_at)
            .cast::<u32>()
            .write_unaligned((count * 2) as u32);
    }
    // SAFETY: live DELETE-capable File, initialized aligned variable-size native
    // structure, bounded byte length; storage and File survive the synchronous call.
    if unsafe {
        SetFileInformationByHandle(
            file.as_raw_handle(),
            FileRenameInfo,
            storage.words.as_ptr().cast(),
            packet_bytes as u32,
        )
    } == 0
    {
        return Err(format!(
            "Owned destination exists or native rename failed (OS {:?}); files retained",
            std::io::Error::last_os_error().raw_os_error()
        ));
    }
    Ok(())
}
#[cfg(unix)]
pub(crate) fn publish_legacy_new(from: &Path, to: &Path) -> Result<(), String> {
    let parent = from
        .parent()
        .filter(|parent| to.parent() == Some(*parent))
        .ok_or("New file publication requires staging beside the destination")?;
    let directory = open_directory(parent)?;
    let from = from.file_name().ok_or("Save temporary name missing")?;
    let to = to.file_name().ok_or("Destination name missing")?;
    rename_new_at(&directory, &directory, from, to)
}

#[cfg(unix)]
fn rename_new(
    source_directory: &File,
    target_directory: &File,
    _path: &Path,
    from: &std::ffi::OsStr,
    to: &std::ffi::OsStr,
    _file: &File,
    _budget: &ResidentBudget,
) -> Result<(), String> {
    rename_new_at(source_directory, target_directory, from, to)
}

#[cfg(unix)]
fn rename_new_at(
    source_directory: &File,
    target_directory: &File,
    from: &std::ffi::OsStr,
    to: &std::ffi::OsStr,
) -> Result<(), String> {
    use std::os::{fd::AsRawFd, unix::ffi::OsStrExt};
    let from = std::ffi::CString::new(from.as_bytes()).map_err(|_| "Invalid owned source name")?;
    let to = std::ffi::CString::new(to.as_bytes()).map_err(|_| "Invalid owned destination name")?;
    #[cfg(target_os = "linux")]
    let result = unsafe {
        libc::renameat2(
            source_directory.as_raw_fd(),
            from.as_ptr(),
            target_directory.as_raw_fd(),
            to.as_ptr(),
            libc::RENAME_NOREPLACE,
        )
    };
    #[cfg(target_os = "macos")]
    let result = unsafe {
        libc::renameatx_np(
            source_directory.as_raw_fd(),
            from.as_ptr(),
            target_directory.as_raw_fd(),
            to.as_ptr(),
            libc::RENAME_EXCL,
        )
    };
    if result != 0 {
        return Err("Owned destination exists or exclusive rename failed; files retained".into());
    }
    Ok(())
}
#[cfg(windows)]
pub(crate) fn read_at(file: &File, mut at: u64, mut output: &mut [u8]) -> Result<(), String> {
    use std::os::windows::fs::FileExt;
    while !output.is_empty() {
        let n = file
            .seek_read(output, at)
            .map_err(|_| "Owned staging read failed")?;
        if n == 0 {
            return Err("Owned staging truncated".into());
        }
        at += n as u64;
        output = &mut output[n..];
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::large_document::ResidentBudget;
    use std::io::Write;
    struct Fixture {
        path: std::path::PathBuf,
        held: Option<File>,
        identity: (u64, u64),
    }
    impl Fixture {
        fn new() -> Self {
            let path = std::env::temp_dir()
                .join(format!("peerbrush-owned-rename-{}", uuid::Uuid::new_v4()));
            fs::create_dir(&path).unwrap();
            let path = fs::canonicalize(path).unwrap();
            let file = open_directory(&path).unwrap();
            let identity = super::super::identity(&file).unwrap();
            Self {
                path,
                held: Some(file),
                identity,
            }
        }
        fn staged(&self) -> (File, Sealed, ResidentBudget) {
            let bytes = b"exact owned candidate bytes";
            let mut file = staging_options()
                .open(self.path.join("candidate.tmp"))
                .unwrap();
            file.try_lock().unwrap();
            file.write_all(bytes).unwrap();
            file.sync_all().unwrap();
            let pool = ResidentBudget::new(CHUNK).unwrap();
            let hash = ring::digest::digest(&SHA256, bytes)
                .as_ref()
                .try_into()
                .unwrap();
            let sealed =
                Sealed::verify(&file, bytes.len() as u64, hash, &pool, &mut || Ok(())).unwrap();
            assert_eq!(pool.used(), 0);
            (file, sealed, pool)
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            if self
                .path
                .starts_with(fs::canonicalize(std::env::temp_dir()).unwrap())
                && self
                    .path
                    .file_name()
                    .is_some_and(|n| n.to_string_lossy().starts_with("peerbrush-owned-rename-"))
                && fs::canonicalize(&self.path).is_ok_and(|p| p == self.path)
                && self
                    .held
                    .as_ref()
                    .is_some_and(|f| super::super::identity(f).is_ok_and(|id| id == self.identity))
                && open_directory(&self.path)
                    .and_then(|f| super::super::identity(&f))
                    .is_ok_and(|id| id == self.identity)
            {
                drop(self.held.take());
                let _ = fs::remove_dir_all(&self.path);
            }
        }
    }
    #[test]
    fn relocation_between_held_directories_retains_the_physical_file_and_metadata() {
        let f = Fixture::new();
        let versions = f.path.join(".peerbrush-versions");
        fs::create_dir(&versions).unwrap();
        let held_versions = open_directory(&versions).unwrap();
        let source = f.path.join("original.psb");
        let bytes = b"original main data";
        fs::write(&source, bytes).unwrap();
        #[cfg(windows)]
        fs::write(
            source.with_file_name("original.psb:peerbrush-metadata"),
            b"retained named stream",
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&source, fs::Permissions::from_mode(0o640)).unwrap();
        }
        let file = staging_options().create_new(false).open(&source).unwrap();
        let pool = ResidentBudget::new(CHUNK).unwrap();
        let hash = ring::digest::digest(&SHA256, bytes)
            .as_ref()
            .try_into()
            .unwrap();
        let sealed =
            Sealed::verify(&file, bytes.len() as u64, hash, &pool, &mut || Ok(())).unwrap();
        let before = file.metadata().unwrap();
        let identity = super::super::identity(&file).unwrap();
        relocate_new(
            &f.path,
            f.held.as_ref().unwrap(),
            &versions,
            &held_versions,
            Path::new("original.psb"),
            Path::new("original.psb"),
            &file,
            &sealed,
            &mut || Ok(()),
        )
        .unwrap();
        let retained = versions.join("original.psb");
        let named = super::super::open_native_identity(&retained).unwrap();
        assert_eq!(super::super::identity(&named).unwrap(), identity);
        assert!(!source.exists());
        assert_eq!(fs::read(&retained).unwrap(), bytes);
        let after = file.metadata().unwrap();
        assert_eq!(before.modified().unwrap(), after.modified().unwrap());
        assert_eq!(before.created().ok(), after.created().ok());
        #[cfg(windows)]
        assert_eq!(
            fs::read(retained.with_file_name("original.psb:peerbrush-metadata")).unwrap(),
            b"retained named stream",
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            assert_eq!(
                (before.uid(), before.gid(), before.mode()),
                (after.uid(), after.gid(), after.mode())
            );
        }
        assert_eq!(pool.used(), 0);
    }
    #[test]
    fn relocation_never_clobbers_an_initial_or_last_callback_destination() {
        for late in [false, true] {
            let f = Fixture::new();
            let (file, sealed, pool) = f.staged();
            let versions = f.path.join(".peerbrush-versions");
            fs::create_dir(&versions).unwrap();
            let held_versions = open_directory(&versions).unwrap();
            let target = versions.join("retained.psb");
            if !late {
                fs::write(&target, b"foreign original survives").unwrap();
            }
            let mut calls = 0;
            assert!(relocate_new(
                &f.path,
                f.held.as_ref().unwrap(),
                &versions,
                &held_versions,
                Path::new("candidate.tmp"),
                Path::new("retained.psb"),
                &file,
                &sealed,
                &mut || {
                    calls += 1;
                    if late && calls == 2 {
                        fs::write(&target, b"foreign original survives").unwrap();
                    }
                    Ok(())
                },
            )
            .is_err());
            file.unlock().unwrap();
            assert_eq!(
                fs::read(f.path.join("candidate.tmp")).unwrap(),
                b"exact owned candidate bytes"
            );
            assert_eq!(fs::read(&target).unwrap(), b"foreign original survives");
            assert_eq!(pool.used(), 0);
        }
    }
    #[test]
    fn relocation_refuses_cancellation_and_a_mismatched_held_directory_before_mutation() {
        let f = Fixture::new();
        let (file, sealed, pool) = f.staged();
        let versions = f.path.join(".peerbrush-versions");
        fs::create_dir(&versions).unwrap();
        let held_versions = open_directory(&versions).unwrap();
        for cancel_at in [1, 2] {
            let mut calls = 0;
            assert!(relocate_new(
                &f.path,
                f.held.as_ref().unwrap(),
                &versions,
                &held_versions,
                Path::new("candidate.tmp"),
                Path::new("retained.psb"),
                &file,
                &sealed,
                &mut || {
                    calls += 1;
                    if calls == cancel_at {
                        Err("cancel relocation".into())
                    } else {
                        Ok(())
                    }
                },
            )
            .is_err());
        }
        assert!(relocate_new(
            &f.path,
            f.held.as_ref().unwrap(),
            &versions,
            f.held.as_ref().unwrap(),
            Path::new("candidate.tmp"),
            Path::new("retained.psb"),
            &file,
            &sealed,
            &mut || Ok(()),
        )
        .is_err());
        file.unlock().unwrap();
        assert_eq!(
            fs::read(f.path.join("candidate.tmp")).unwrap(),
            b"exact owned candidate bytes"
        );
        assert!(!versions.join("retained.psb").exists());
        assert_eq!(pool.used(), 0);
    }
    #[cfg(windows)]
    #[test]
    fn relocation_pins_both_directory_names_through_the_last_callback() {
        let f = Fixture::new();
        let (file, sealed, pool) = f.staged();
        let versions = f.path.join(".peerbrush-versions");
        fs::create_dir(&versions).unwrap();
        let held_versions = open_directory(&versions).unwrap();
        let moved = f.path.with_file_name(format!(
            "peerbrush-owned-rename-moved-{}",
            uuid::Uuid::new_v4()
        ));
        let mut calls = 0;
        relocate_new(
            &f.path,
            f.held.as_ref().unwrap(),
            &versions,
            &held_versions,
            Path::new("candidate.tmp"),
            Path::new("retained.psb"),
            &file,
            &sealed,
            &mut || {
                calls += 1;
                if calls == 2 {
                    for (from, to) in [
                        (&f.path, &moved),
                        (&versions, &f.path.join("moved-versions")),
                    ] {
                        let error = fs::rename(from, to).unwrap_err();
                        assert!(matches!(error.raw_os_error(), Some(5 | 32)));
                    }
                }
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(calls, 2);
        file.unlock().unwrap();
        assert_eq!(
            fs::read(versions.join("retained.psb")).unwrap(),
            b"exact owned candidate bytes"
        );
        assert!(!moved.exists());
        assert_eq!(pool.used(), 0);
    }
    #[test]
    fn absolute_native_rename_covers_every_utf16_packet_alignment() {
        for length in 1..=8 {
            let f = Fixture::new();
            let (file, sealed, pool) = f.staged();
            let name = "x".repeat(length);
            publish_new(
                &f.path,
                f.held.as_ref().unwrap(),
                Path::new("candidate.tmp"),
                Path::new(&name),
                &file,
                &sealed,
                &mut || Ok(()),
            )
            .unwrap();
            assert_eq!(pool.used(), 0);
            file.unlock().unwrap();
            assert_eq!(
                fs::read(f.path.join(name)).unwrap(),
                b"exact owned candidate bytes"
            );
        }
    }
    #[test]
    fn held_verified_candidate_publishes_once_without_an_old_path_fallback() {
        let f = Fixture::new();
        let (file, sealed, _pool) = f.staged();
        publish_new(
            &f.path,
            f.held.as_ref().unwrap(),
            Path::new("candidate.tmp"),
            Path::new("published.json"),
            &file,
            &sealed,
            &mut || Ok(()),
        )
        .unwrap();
        assert!(!f.path.join("candidate.tmp").exists());
        assert!(publish_new(
            &f.path,
            f.held.as_ref().unwrap(),
            Path::new("candidate.tmp"),
            Path::new("published.json"),
            &file,
            &sealed,
            &mut || Ok(())
        )
        .is_err());
        file.unlock().unwrap();
        assert_eq!(
            fs::read(f.path.join("published.json")).unwrap(),
            b"exact owned candidate bytes"
        );
    }
    #[test]
    fn late_foreign_destination_and_source_are_preserved_on_refusal() {
        for destination in [false, true] {
            let f = Fixture::new();
            let (file, sealed, _pool) = f.staged();
            let mut calls = 0;
            let result = publish_new(
                &f.path,
                f.held.as_ref().unwrap(),
                Path::new("candidate.tmp"),
                Path::new("published.json"),
                &file,
                &sealed,
                &mut || {
                    calls += 1;
                    if calls == 2 {
                        if destination {
                            fs::write(f.path.join("published.json"), b"foreign destination")
                                .unwrap();
                        } else {
                            fs::rename(f.path.join("candidate.tmp"), f.path.join("retained.tmp"))
                                .unwrap();
                            fs::write(f.path.join("candidate.tmp"), b"foreign source").unwrap();
                        }
                    }
                    Ok(())
                },
            );
            assert!(result.is_err());
            file.unlock().unwrap();
            if destination {
                assert_eq!(
                    fs::read(f.path.join("published.json")).unwrap(),
                    b"foreign destination"
                );
                assert_eq!(
                    fs::read(f.path.join("candidate.tmp")).unwrap(),
                    b"exact owned candidate bytes"
                );
            } else {
                assert!(!f.path.join("published.json").exists());
                assert_eq!(
                    fs::read(f.path.join("candidate.tmp")).unwrap(),
                    b"foreign source"
                );
                assert_eq!(
                    fs::read(f.path.join("retained.tmp")).unwrap(),
                    b"exact owned candidate bytes"
                );
            }
        }
    }
    #[test]
    fn cancellation_corrupt_bytes_and_cross_directory_names_never_publish() {
        let f = Fixture::new();
        let (file, sealed, pool) = f.staged();
        assert!(publish_new(
            &f.path,
            f.held.as_ref().unwrap(),
            Path::new("candidate.tmp"),
            Path::new("../foreign"),
            &file,
            &sealed,
            &mut || Ok(())
        )
        .is_err());
        assert!(publish_new(
            &f.path,
            f.held.as_ref().unwrap(),
            Path::new("candidate.tmp"),
            Path::new("published.json"),
            &file,
            &sealed,
            &mut || Err("cancel".into())
        )
        .is_err());
        assert!(Sealed::verify(&file, sealed.bytes, [7; 32], &pool, &mut || Ok(())).is_err());
        assert!(Sealed::verify(&file, MAXIMUM + 1, [0; 32], &pool, &mut || Ok(())).is_err());
        assert!(f.path.join("candidate.tmp").exists());
        assert!(!f.path.join("published.json").exists());
    }
    #[test]
    fn native_maximum_component_works_beyond_a_legacy_windows_path_length() {
        let f = Fixture::new();
        let (file, sealed, _pool) = f.staged();
        let name = format!("{}.json", "a".repeat(250));
        assert_eq!(name.len(), 255);
        publish_new(
            &f.path,
            f.held.as_ref().unwrap(),
            Path::new("candidate.tmp"),
            Path::new(&name),
            &file,
            &sealed,
            &mut || Ok(()),
        )
        .unwrap();
        file.unlock().unwrap();
        assert_eq!(
            fs::read(f.path.join(name)).unwrap(),
            b"exact owned candidate bytes"
        );
    }
    #[test]
    fn native_buffer_refusal_retains_names_and_restores_exact_shared_capacity() {
        let f = Fixture::new();
        let (file, sealed, pool) = f.staged();
        assert_eq!(pool.used(), 0);
        let held = pool.reserve_bytes(CHUNK, CHUNK).unwrap();
        #[cfg(windows)]
        {
            assert!(publish_new(
                &f.path,
                f.held.as_ref().unwrap(),
                Path::new("candidate.tmp"),
                Path::new("published.json"),
                &file,
                &sealed,
                &mut || Ok(())
            )
            .is_err());
            assert!(f.path.join("candidate.tmp").exists());
            assert!(!f.path.join("published.json").exists());
        }
        assert_eq!(pool.used(), CHUNK);
        drop(held);
        assert_eq!(pool.used(), 0);
        publish_new(
            &f.path,
            f.held.as_ref().unwrap(),
            Path::new("candidate.tmp"),
            Path::new("published.json"),
            &file,
            &sealed,
            &mut || Ok(()),
        )
        .unwrap();
        assert_eq!(pool.used(), 0);
    }
    #[cfg(windows)]
    #[test]
    fn final_callback_cannot_replace_the_pinned_destination_directory() {
        let f = Fixture::new();
        let (file, sealed, pool) = f.staged();
        let moved = f.path.with_file_name(format!(
            "peerbrush-owned-rename-moved-{}",
            uuid::Uuid::new_v4()
        ));
        let mut calls = 0;
        publish_new(
            &f.path,
            f.held.as_ref().unwrap(),
            Path::new("candidate.tmp"),
            Path::new("published.json"),
            &file,
            &sealed,
            &mut || {
                calls += 1;
                if calls == 2 {
                    let error = fs::rename(&f.path, &moved).unwrap_err();
                    assert!(matches!(error.raw_os_error(), Some(5 | 32)));
                }
                Ok(())
            },
        )
        .unwrap();
        assert!(!moved.exists());
        assert!(f.path.join("published.json").exists());
        assert_eq!(pool.used(), 0);
    }
}
#[cfg(unix)]
pub(crate) fn read_at(file: &File, mut at: u64, mut output: &mut [u8]) -> Result<(), String> {
    use std::os::unix::fs::FileExt;
    while !output.is_empty() {
        let n = file
            .read_at(output, at)
            .map_err(|_| "Owned staging read failed")?;
        if n == 0 {
            return Err("Owned staging truncated".into());
        }
        at += n as u64;
        output = &mut output[n..];
    }
    Ok(())
}
