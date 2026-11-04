//! Held-file PNG transport. It borrows the genuine session for its entire
//! lifetime and never materializes the encoded file or exposes arbitrary reads.
use super::{
    container_stream, decode_input, ColorPolicy, Complete, Header, PassRow, SourceBinding,
};
use crate::{
    large_document::{root::Directory, Reservation, ScratchBytes, MAX_SOURCE_PAYLOAD},
    workspace::source_runtime::SourceSession,
};
use ring::digest::{Context, SHA256};
use std::{
    fs::{self, File, OpenOptions},
    io::{self, BufRead, Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    time::SystemTime,
};

#[derive(Clone, Copy)]
pub(crate) struct FileLimits {
    pub edge: u32,
    pub pixels: u64,
    pub encoded_bytes: u64,
}
impl FileLimits {
    pub(crate) const SELECTION: Self = Self {
        edge: 8192,
        pixels: 16_000_000,
        encoded_bytes: 64 * 1024 * 1024,
    };
    fn validate(self) -> Result<(), String> {
        if self.edge == 0
            || self.edge > 16000
            || self.pixels == 0
            || self.pixels > 256_000_000
            || self.encoded_bytes == 0
            || self.encoded_bytes > MAX_SOURCE_PAYLOAD
        {
            return Err("Invalid held PNG source admission limits".into());
        }
        Ok(())
    }
}

pub(crate) struct FilePng<'a> {
    session: &'a SourceSession,
    directory: Directory,
    path: PathBuf,
    file: File,
    identity: (u64, u64),
    length: u64,
    modified: SystemTime,
    facts: (u32, u32, bool),
    color: ColorPolicy,
    binding: SourceBinding,
    buffer: ScratchBytes,
    first: usize,
    end: usize,
    read_bytes: u64,
    digest: Option<Context>,
    // Directory/path allocations drop before their conservative admission.
    _paths: Reservation,
}
impl<'a> FilePng<'a> {
    /// The shared command must authorize this user-selected source path before
    /// opening. This accepts PNG only, creates no files, and grants no document
    /// edit/adoption authority. Larger component limits are explicit, not live
    /// selection/canvas defaults.
    pub(crate) fn open(
        session: &'a SourceSession,
        path: &Path,
        limits: FileLimits,
        color: ColorPolicy,
        check: &mut dyn FnMut() -> Result<(), String>,
    ) -> Result<Self, String> {
        session.check()?;
        check()?;
        limits.validate()?;
        if color == ColorPolicy::ToSrgb {
            return Err("Held native PNG sources require raw color policy".into());
        }
        if !path
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("png"))
        {
            return Err("Held PNG source requires an authorized PNG path".into());
        }
        let units = path.as_os_str().len();
        if units == 0 || units > 32768 {
            return Err("Held PNG source path exceeds admission".into());
        }
        // Canonicalizing a short relative path can expand to a long absolute
        // parent. Admit the bounded worst case before asking the OS to allocate.
        let admitted = 32768 * 32 + 8192;
        let paths = session
            .resident()
            .reserve_bytes(admitted, admitted)
            .map_err(|e| e.to_string())?;
        let parent = fs::canonicalize(
            path.parent()
                .filter(|p| !p.as_os_str().is_empty())
                .unwrap_or(Path::new(".")),
        )
        .map_err(|_| "Held PNG source directory unavailable")?;
        if parent.as_os_str().len() + path.file_name().map_or(0, |n| n.len()) + 1 > 32768 {
            return Err("Canonical held PNG path exceeds admission".into());
        }
        let directory = Directory::open(&parent)?;
        let path = parent.join(path.file_name().ok_or("Held PNG source has no file name")?);
        let named = fs::symlink_metadata(&path).map_err(|_| "Held PNG source unavailable")?;
        if !named.is_file() || named.file_type().is_symlink() {
            return Err("Held PNG source must be an ordinary file".into());
        }
        let file = open_source(&path, false)?;
        let metadata = file
            .metadata()
            .map_err(|_| "Held PNG source metadata unavailable")?;
        let length = metadata.len();
        if length == 0 || length > limits.encoded_bytes {
            return Err("Held PNG source bytes exceed admission".into());
        }
        let identity = crate::file_safety::identity(&file)?;
        let modified = metadata
            .modified()
            .map_err(|_| "Held PNG source timestamp unavailable")?;
        let buffer = ScratchBytes::new(&session.resident(), 64 * 1024, 64 * 1024)
            .map_err(|e| e.to_string())?;
        let mut held = Self {
            session,
            directory,
            path,
            file,
            identity,
            length,
            modified,
            facts: (0, 0, false),
            color,
            binding: SourceBinding {
                source: [0; 32],
                transport: None,
                pool: session.resident(),
            },
            buffer,
            first: 0,
            end: 0,
            read_bytes: 0,
            digest: None,
            _paths: paths,
        };
        held.recheck()?;
        held.start_read()?;
        held.facts = container_stream(&mut held, length, limits.edge, limits.pixels, color, check)?;
        held.binding.source = held.finish_read(None, check)?;
        check()?;
        Ok(held)
    }
    pub(in crate::image_import) fn binding(
        &self,
        session: &SourceSession,
    ) -> Result<SourceBinding, String> {
        if !std::ptr::eq(self.session, session) {
            return Err("Held PNG source belongs to a different session".into());
        }
        Ok(self.binding.clone())
    }
    /// Recheck the same opened file, its retained parent directory and the named
    /// identity. Bytes consumed by the decoder are separately hash-bound below.
    pub(crate) fn recheck(&self) -> Result<(), String> {
        self.session.check()?;
        self.directory.check()?;
        let named =
            fs::symlink_metadata(&self.path).map_err(|_| "Held PNG source name disappeared")?;
        if !named.is_file() || named.file_type().is_symlink() {
            return Err("Held PNG source is no longer an ordinary file".into());
        }
        let opened = open_source(&self.path, true)?;
        let actual = self
            .file
            .metadata()
            .map_err(|_| "Held PNG source metadata unavailable")?;
        if !actual.is_file()
            || crate::file_safety::identity(&self.file)? != self.identity
            || crate::file_safety::identity(&opened)? != self.identity
            || crate::file_safety::link_count(&self.file)? != 1
            || crate::file_safety::link_count(&opened)? != 1
            || actual.len() != self.length
            || actual.modified().ok() != Some(self.modified)
        {
            return Err("Held PNG source ownership or stamp changed".into());
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::MetadataExt;
            if named.file_attributes() & 0x400 != 0 {
                return Err("Held PNG source alias refused".into());
            }
        }
        Ok(())
    }
    pub(in crate::image_import) fn decode_pass_rows(
        &mut self,
        check: &mut dyn FnMut() -> Result<(), String>,
        row: impl FnMut(Header, PassRow, &[u8]) -> Result<(), String>,
    ) -> Result<Complete, String> {
        self.recheck()?;
        check()?;
        self.start_read()?;
        let (facts, binding, color, pool) = (
            self.facts,
            self.binding.clone(),
            self.color,
            self.session.resident(),
        );
        let complete = decode_input(&mut *self, facts, binding, color, &pool, check, row)?;
        self.finish_read(Some(self.binding.source), check)?;
        check()?;
        Ok(complete)
    }
    pub(in crate::image_import) fn color_policy(&self) -> ColorPolicy {
        self.color
    }
    fn start_read(&mut self) -> Result<(), String> {
        self.recheck()?;
        self.file
            .seek(SeekFrom::Start(0))
            .map_err(|_| "Cannot rewind held PNG source")?;
        self.first = 0;
        self.end = 0;
        self.read_bytes = 0;
        self.digest = Some(Context::new(&SHA256));
        Ok(())
    }
    fn finish_read(
        &mut self,
        expected: Option<[u8; 32]>,
        check: &mut dyn FnMut() -> Result<(), String>,
    ) -> Result<[u8; 32], String> {
        // The decoder may stop at IEND while its buffered transport still owns
        // bytes. Drain the same monotonic stream, never rehash a different read.
        loop {
            check()?;
            let length = self.fill_buf().map_err(|e| e.to_string())?.len();
            if length == 0 {
                break;
            }
            self.consume(length);
        }
        self.recheck()?;
        if self.read_bytes != self.length {
            return Err("Held PNG source byte count changed".into());
        }
        let actual: [u8; 32] = self
            .digest
            .take()
            .ok_or("Held PNG source read is incomplete")?
            .finish()
            .as_ref()
            .try_into()
            .unwrap();
        if expected.is_some_and(|hash| hash != actual) {
            return Err("Bytes consumed by the PNG decoder differ from the admitted source".into());
        }
        Ok(actual)
    }
}

#[cfg(unix)]
const INPUT_FLAGS: i32 = libc::O_NONBLOCK | libc::O_NOFOLLOW | libc::O_CLOEXEC;

/// A named regular file can become a FIFO between lstat and open. Both initial
/// data admission and identity rechecks must remain nonblocking, then validate
/// the actual opened type before any read. Never use the general identity-open
/// helper here: its Unix read-open can block on an untrusted replacement.
fn open_source(path: &Path, identity_only: bool) -> Result<File, String> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.custom_flags(0x00200000);
        if identity_only {
            options.access_mode(0).share_mode(7);
        } else {
            options.share_mode(1);
        }
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        let _ = identity_only;
        options.custom_flags(INPUT_FLAGS);
    }
    let file = options
        .open(path)
        .map_err(|_| "Cannot hold the selected ordinary PNG source")?;
    let metadata = file
        .metadata()
        .map_err(|_| "Held PNG source metadata unavailable")?;
    if !metadata.is_file() {
        return Err("Opened PNG source is not an ordinary file".into());
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if metadata.file_attributes() & 0x400 != 0 {
            return Err("Opened PNG source alias refused".into());
        }
    }
    Ok(file)
}

#[cfg(all(test, unix))]
mod unix_tests {
    use super::*;
    use std::{
        ffi::CString,
        os::unix::{ffi::OsStrExt, io::AsRawFd},
    };
    #[test]
    fn actual_open_refuses_a_fifo_replacing_a_previously_checked_regular_file() {
        // Assert the actual helper's flag before attempting FIFO opens, so an
        // accidental removal fails instead of leaving the regression test hung.
        assert_ne!(INPUT_FLAGS & libc::O_NONBLOCK, 0);
        let parent = fs::canonicalize(std::env::temp_dir()).unwrap();
        let root = parent.join(format!("pb-png-fifo-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&root).unwrap();
        let held = Directory::open(&root).unwrap();
        let path = root.join("source.png");
        fs::write(&path, b"ordinary").unwrap();
        let checked = fs::symlink_metadata(&path).unwrap();
        let original = open_source(&path, false).unwrap();
        // SAFETY: a live file descriptor is queried without ownership transfer.
        let flags = unsafe { libc::fcntl(original.as_raw_fd(), libc::F_GETFL) };
        assert!(
            flags >= 0,
            "Cannot query the actual source descriptor flags"
        );
        assert_ne!(flags & libc::O_NONBLOCK, 0);
        fs::remove_file(&path).unwrap();
        let cpath = CString::new(path.as_os_str().as_bytes()).unwrap();
        // SAFETY: the NUL-terminated path is owned by this private fixture.
        assert_eq!(unsafe { libc::mkfifo(cpath.as_ptr(), 0o600) }, 0);
        assert!(checked.is_file());
        assert!(open_source(&path, false).is_err());
        assert!(open_source(&path, true).is_err());
        assert_eq!(original.metadata().unwrap().len(), 8);
        drop(original);
        held.check().unwrap();
        fs::remove_file(&path).unwrap();
        drop(held);
        fs::remove_dir(&root).unwrap();
    }
}
impl BufRead for FilePng<'_> {
    fn fill_buf(&mut self) -> io::Result<&[u8]> {
        self.session.check().map_err(io::Error::other)?;
        if self.first == self.end {
            self.first = 0;
            self.end = self.file.read(&mut self.buffer)?;
            self.read_bytes = self
                .read_bytes
                .checked_add(self.end as u64)
                .filter(|n| *n <= self.length)
                .ok_or_else(|| io::Error::other("Held PNG source grew"))?;
            self.digest
                .as_mut()
                .ok_or_else(|| io::Error::other("Held PNG read was not admitted"))?
                .update(&self.buffer[..self.end]);
        }
        Ok(&self.buffer[self.first..self.end])
    }
    fn consume(&mut self, amount: usize) {
        self.first = (self.first + amount.min(self.end - self.first)).min(self.end);
    }
}
impl Read for FilePng<'_> {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        if output.is_empty() {
            return Ok(0);
        }
        let bytes = self.fill_buf()?;
        let count = bytes.len().min(output.len());
        output[..count].copy_from_slice(&bytes[..count]);
        self.consume(count);
        Ok(count)
    }
}
impl Seek for FilePng<'_> {
    fn seek(&mut self, to: SeekFrom) -> io::Result<u64> {
        let current = self.read_bytes - (self.end - self.first) as u64;
        match to {
            SeekFrom::Current(0) => Ok(current),
            SeekFrom::Start(position) if position == current => Ok(current),
            // png0.18.1 production decoding is monotonic. Refuse an unexpected
            // seek rather than silently invalidating the consumed-byte proof.
            _ => Err(io::Error::other(
                "PNG decoder requested an unadmitted source seek",
            )),
        }
    }
}
