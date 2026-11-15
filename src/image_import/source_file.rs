//! Held, hash-bound image input shared by streaming decoders. Reads stay
//! monotonic and retain the actual file/session ownership until completion.
use super::prepared_png::SourceBinding;
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
            return Err("Invalid held image source admission limits".into());
        }
        Ok(())
    }
}

pub(crate) struct FileSource<'a> {
    session: &'a SourceSession,
    directory: Directory,
    path: PathBuf,
    file: File,
    identity: (u64, u64),
    length: u64,
    modified: SystemTime,
    expected: Option<[u8; 32]>,
    binding: Option<SourceBinding>,
    buffer: ScratchBytes,
    first: usize,
    end: usize,
    read_bytes: u64,
    digest: Option<Context>,
    // Directory/path allocations drop before their conservative admission.
    _paths: Reservation,
}
impl<'a> FileSource<'a> {
    pub(super) fn source_binding(&self) -> Result<SourceBinding, String> {
        self.binding
            .clone()
            .ok_or_else(|| "Image source has not completed admission".into())
    }
    pub(super) fn pool(&self) -> crate::large_document::ResidentBudget {
        self.session.resident()
    }
    /// The shared command must authorize this user-selected source path before
    /// opening. This checks the selected codec extension and grants no document
    /// edit/adoption authority. Larger component limits are explicit, not live
    /// selection/canvas defaults.
    pub(crate) fn open(
        session: &'a SourceSession,
        path: &Path,
        limits: FileLimits,
        extension: &str,
        check: &mut dyn FnMut() -> Result<(), String>,
    ) -> Result<Self, String> {
        Self::open_with(session, path, limits, extension, None, None, check)
    }

    pub(super) fn open_held(
        session: &'a SourceSession,
        path: &Path,
        limits: FileLimits,
        extension: &str,
        held: (File, [u8; 32]),
        check: &mut dyn FnMut() -> Result<(), String>,
    ) -> Result<Self, String> {
        Self::open_with(session, path, limits, extension, Some(held.0), Some(held.1), check)
    }

    pub(super) fn open_pinned(
        session: &'a SourceSession,
        path: &Path,
        limits: FileLimits,
        extension: &str,
        expected: [u8; 32],
        check: &mut dyn FnMut() -> Result<(), String>,
    ) -> Result<Self, String> {
        Self::open_with(session, path, limits, extension, None, Some(expected), check)
    }

    fn open_with(
        session: &'a SourceSession,
        path: &Path,
        limits: FileLimits,
        extension: &str,
        held: Option<File>,
        expected: Option<[u8; 32]>,
        check: &mut dyn FnMut() -> Result<(), String>,
    ) -> Result<Self, String> {
        session.check()?;
        check()?;
        limits.validate()?;
        if !path.is_absolute() {
            return Err("Use an absolute local image path".into());
        }
        if !path
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case(extension))
        {
            return Err("Held image source extension differs from the selected decoder".into());
        }
        let units = path.as_os_str().len();
        if units == 0 || units > 32768 {
            return Err("Held image source path exceeds admission".into());
        }
        // Canonicalizing aliases can expand the parent. Admit the bounded worst
        // case before asking the OS to allocate.
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
        .map_err(|_| "Held image source directory unavailable")?;
        if parent.as_os_str().len() + path.file_name().map_or(0, |n| n.len()) + 1 > 32768 {
            return Err("Canonical held image path exceeds admission".into());
        }
        let directory = Directory::open(&parent)?;
        let path = parent.join(
            path.file_name()
                .ok_or("Held image source has no file name")?,
        );
        let named = fs::symlink_metadata(&path).map_err(|_| "Held image source unavailable")?;
        if !named.is_file() || named.file_type().is_symlink() {
            return Err("Held image source must be an ordinary file".into());
        }
        let file = match held {
            Some(file) => file,
            None => open_source(&path, false)?,
        };
        let metadata = file
            .metadata()
            .map_err(|_| "Held image source metadata unavailable")?;
        let length = metadata.len();
        if length == 0 || length > limits.encoded_bytes {
            return Err("Held image source bytes exceed admission".into());
        }
        let identity = crate::file_safety::identity(&file)?;
        let modified = metadata
            .modified()
            .map_err(|_| "Held image source timestamp unavailable")?;
        let buffer = ScratchBytes::new(&session.resident(), 64 * 1024, 64 * 1024)
            .map_err(|e| e.to_string())?;
        let held = Self {
            session,
            directory,
            path,
            file,
            identity,
            length,
            modified,
            expected,
            binding: None,
            buffer,
            first: 0,
            end: 0,
            read_bytes: 0,
            digest: None,
            _paths: paths,
        };
        held.recheck()?;
        check()?;
        Ok(held)
    }
    pub(in crate::image_import) fn binding(
        &self,
        session: &SourceSession,
    ) -> Result<SourceBinding, String> {
        if !std::ptr::eq(self.session, session) {
            return Err("Held image source belongs to a different session".into());
        }
        self.source_binding()
    }
    /// Recheck the same opened file, its retained parent directory and the named
    /// identity. Bytes consumed by the decoder are separately hash-bound below.
    pub(crate) fn recheck(&self) -> Result<(), String> {
        self.session.check()?;
        self.directory.check()?;
        let named =
            fs::symlink_metadata(&self.path).map_err(|_| "Held image source name disappeared")?;
        if !named.is_file() || named.file_type().is_symlink() {
            return Err("Held image source is no longer an ordinary file".into());
        }
        let opened = open_source(&self.path, true)?;
        let actual = self
            .file
            .metadata()
            .map_err(|_| "Held image source metadata unavailable")?;
        if !actual.is_file()
            || crate::file_safety::identity(&self.file)? != self.identity
            || crate::file_safety::identity(&opened)? != self.identity
            || crate::file_safety::link_count(&self.file)? != 1
            || crate::file_safety::link_count(&opened)? != 1
            || actual.len() != self.length
            || actual.modified().ok() != Some(self.modified)
        {
            return Err("Held image source ownership or stamp changed".into());
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::MetadataExt;
            if named.file_attributes() & 0x400 != 0 {
                return Err("Held image source alias refused".into());
            }
        }
        Ok(())
    }
    pub(super) fn length(&self) -> u64 {
        self.length
    }
    /// Read held metadata before hashing. A successful probe grants no source binding.
    pub(super) fn inspect_seekable(
        &mut self,
        inspect: impl FnOnce(&mut File, u64) -> Result<(), String>,
    ) -> Result<(), String> {
        self.binding = None;
        self.recheck()?;
        let result = inspect(&mut self.file, self.length);
        self.start_read()?;
        result
    }
    pub(super) fn buffered(&self) -> &[u8] {
        &self.buffer[self.first..self.end]
    }
    /// A sequential decoder with its own bounded work checks can consume
    /// bytes already held in the buffer. Each refill still checks the source.
    pub(super) fn decoder_stream(&mut self) -> DecoderStream<'_, 'a> {
        DecoderStream(self)
    }
    fn read_buffered(&mut self, output: &mut [u8], check_cached: bool) -> io::Result<usize> {
        if output.is_empty() {
            return Ok(0);
        }
        let bytes = if check_cached || self.first == self.end {
            self.fill_buf()?
        } else {
            self.buffered()
        };
        let count = bytes.len().min(output.len());
        output[..count].copy_from_slice(&bytes[..count]);
        self.consume(count);
        Ok(count)
    }
    pub(super) fn skip(
        &mut self,
        mut bytes: u64,
        check: &mut dyn FnMut() -> Result<(), String>,
    ) -> Result<(), String> {
        while bytes != 0 {
            check()?;
            let count = self
                .fill_buf()
                .map_err(|e| e.to_string())?
                .len()
                .min(bytes as usize);
            if count == 0 {
                return Err("Truncated held image source offset".into());
            }
            self.consume(count);
            bytes -= count as u64;
        }
        Ok(())
    }
    pub(super) fn admit(
        &mut self,
        check: &mut dyn FnMut() -> Result<(), String>,
    ) -> Result<(), String> {
        let hash = self.finish_read(self.expected, check)?;
        self.binding = Some(SourceBinding::file(hash, self.session.resident()));
        Ok(())
    }
    pub(super) fn finish(
        &mut self,
        check: &mut dyn FnMut() -> Result<(), String>,
    ) -> Result<(), String> {
        self.finish_read(Some(self.source_binding()?.source_sha256()), check)
            .map(|_| ())
    }
    pub(super) fn start_read(&mut self) -> Result<(), String> {
        self.recheck()?;
        self.file
            .seek(SeekFrom::Start(0))
            .map_err(|_| "Cannot rewind held image source")?;
        self.first = 0;
        self.end = 0;
        self.read_bytes = 0;
        self.digest = Some(Context::new(&SHA256));
        Ok(())
    }
    /// Snapshot seek-based codecs into the session's authenticated byte spool.
    /// A pinned source verifies the copied bytes against its independent digest.
    /// Unpinned files first establish the digest with a separate complete read.
    pub(super) fn snapshot(
        &mut self,
        check: &mut dyn FnMut() -> Result<(), String>,
    ) -> Result<crate::workspace::source_runtime::SessionBytes<'a>, String> {
        self.binding = None;
        self.start_read()?;
        let binding = match self.expected {
            Some(expected) => expected,
            None => {
                let observed = self.finish_read(None, check)?;
                self.start_read()?;
                observed
            }
        };
        check()?;
        let mut temporary = self
            .session
            .byte_temporary(self.length, crate::large_document::ByteRole::SourceBulk)?;
        let copied = (|| {
            loop {
                check()?;
                let bytes = self.fill_buf().map_err(|e| e.to_string())?;
                if bytes.is_empty() {
                    break;
                }
                let count = bytes.len();
                temporary.append(bytes, check)?;
                self.consume(count);
            }
            self.finish_read(Some(binding), check).map(|_| ())
        })();
        if let Err(reason) = copied {
            return match temporary.abort(&mut || self.session.cleanup_check()) {
                Ok(()) => Err(reason),
                Err(cleanup) => Err(format!(
                    "{reason}; encoded source retirement refused: {cleanup}"
                )),
            };
        }
        // Interrupted seals retire only after proving their exact written bytes.
        // Changed or partially unknown data remains inventoried, never decoded.
        let bytes = temporary.finish(binding, check)?;
        if let Err(reason) = self.recheck().and_then(|()| check()) {
            return match bytes.retire(&mut || self.session.cleanup_check()) {
                Ok(()) => Err(reason),
                Err(cleanup) => Err(format!(
                    "{reason}; encoded source retirement refused: {cleanup}"
                )),
            };
        }
        self.binding = Some(SourceBinding::file(binding, self.session.resident()));
        Ok(bytes)
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
            return Err("Held image source byte count changed".into());
        }
        let actual: [u8; 32] = self
            .digest
            .take()
            .ok_or("Held image source read is incomplete")?
            .finish()
            .as_ref()
            .try_into()
            .unwrap();
        if expected.is_some_and(|hash| hash != actual) {
            return Err(
                "Bytes consumed by the image decoder differ from the admitted source".into(),
            );
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
        .map_err(|_| "Cannot hold the selected ordinary image source")?;
    let metadata = file
        .metadata()
        .map_err(|_| "Held image source metadata unavailable")?;
    if !metadata.is_file() {
        return Err("Opened image source is not an ordinary file".into());
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if metadata.file_attributes() & 0x400 != 0 {
            return Err("Opened image source alias refused".into());
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
impl BufRead for FileSource<'_> {
    fn fill_buf(&mut self) -> io::Result<&[u8]> {
        self.session.check().map_err(io::Error::other)?;
        if self.first == self.end {
            self.first = 0;
            self.end = self.file.read(&mut self.buffer)?;
            self.read_bytes = self
                .read_bytes
                .checked_add(self.end as u64)
                .filter(|n| *n <= self.length)
                .ok_or_else(|| io::Error::other("Held image source grew"))?;
            self.digest
                .as_mut()
                .ok_or_else(|| io::Error::other("Held image read was not admitted"))?
                .update(&self.buffer[..self.end]);
        }
        Ok(&self.buffer[self.first..self.end])
    }
    fn consume(&mut self, amount: usize) {
        self.first = (self.first + amount.min(self.end - self.first)).min(self.end);
    }
}
impl Read for FileSource<'_> {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        self.read_buffered(output, true)
    }
}

pub(super) struct DecoderStream<'r, 'a>(&'r mut FileSource<'a>);

impl Read for DecoderStream<'_, '_> {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        self.0.read_buffered(output, false)
    }
}
impl BufRead for DecoderStream<'_, '_> {
    fn fill_buf(&mut self) -> io::Result<&[u8]> {
        if self.0.first == self.0.end {
            self.0.fill_buf()
        } else {
            Ok(self.0.buffered())
        }
    }
    fn consume(&mut self, amount: usize) {
        self.0.consume(amount);
    }
}
impl Seek for DecoderStream<'_, '_> {
    fn seek(&mut self, to: SeekFrom) -> io::Result<u64> {
        self.0.seek(to)
    }
}
impl Seek for FileSource<'_> {
    fn seek(&mut self, to: SeekFrom) -> io::Result<u64> {
        let current = self.read_bytes - (self.end - self.first) as u64;
        match to {
            SeekFrom::Current(0) => Ok(current),
            SeekFrom::Start(position) if position == current => Ok(current),
            // Streaming decoders are monotonic. Refuse an unexpected
            // seek rather than silently invalidating the consumed-byte proof.
            _ => Err(io::Error::other(
                "Image decoder requested an unadmitted source seek",
            )),
        }
    }
}

#[cfg(test)]
mod snapshot_tests {
    use super::*;
    use crate::engine::native::worker::tests::Fixture;

    #[test]
    fn pinned_snapshots_verify_the_copy_without_a_second_admission_scan() {
        let root = Fixture::new();
        let (runtime, connection) = root.runtime_with_budgets(16 * 1024 * 1024, 32 * 1024 * 1024);
        let session = runtime.begin().unwrap();
        let path = root.path().join("pinned.tif");
        let data = vec![37; 3 * 64 * 1024 + 7];
        std::fs::write(&path, &data).unwrap();
        let hash: [u8; 32] = ring::digest::digest(&SHA256, &data).as_ref().try_into().unwrap();
        let limits = FileLimits { edge: 8192, pixels: 8192 * 8192, encoded_bytes: 128 * 1024 * 1024 };
        let disk = runtime.disk().used().unwrap();
        let resident = runtime.resident().used();

        for (source, pinned) in [
            (FileSource::open(&session, &path, limits, "tif", &mut || Ok(())).unwrap(), false),
            (FileSource::open_pinned(&session, &path, limits, "tif", hash, &mut || Ok(())).unwrap(), true),
            (FileSource::open_held(&session, &path, limits, "tif", (open_source(&path, false).unwrap(), hash),
                &mut || Ok(())).unwrap(), true),
        ] {
            let mut source = source;
            source.inspect_seekable(|file, length| {
                assert_eq!(length, data.len() as u64);
                file.seek(SeekFrom::End(-1)).map_err(|e| e.to_string())?;
                let mut tail = [0];
                file.read_exact(&mut tail).map_err(|e| e.to_string())?;
                assert_eq!(tail, [37]);
                Ok(())
            }).unwrap();
            assert!(source.source_binding().is_err());
            let mut before_spool = 0;
            let snapshot = source.snapshot(&mut || {
                if runtime.disk().used().unwrap() == disk {
                    before_spool += 1;
                }
                Ok(())
            }).unwrap();
            if pinned {
                assert_eq!(before_spool, 1);
            } else {
                assert!(before_spool > 4);
            }
            assert_eq!(snapshot.bytes().binding(), hash);
            assert_eq!(snapshot.bytes().payload_sha256(), hash);
            assert_eq!(source.source_binding().unwrap().source_sha256(), hash);
            snapshot.retire(&mut || session.cleanup_check()).unwrap();
        }

        let mut wrong = hash;
        wrong[0] ^= 1;
        let mut source = FileSource::open_pinned(&session, &path, limits, "tif", wrong, &mut || Ok(())).unwrap();
        let reason = source.snapshot(&mut || Ok(())).err().unwrap();
        assert!(reason.contains("differ from the admitted source"), "{reason}");
        assert!(source.source_binding().is_err());
        assert_eq!(runtime.disk().used().unwrap(), disk);
        drop(source);
        assert_eq!(runtime.resident().used(), resident);
        drop((session, runtime, connection));
    }

    #[test]
    fn encoded_snapshot_cancellation_including_sealing_retires_only_verified_data() {
        let root = Fixture::new();
        let (runtime, connection) = root.runtime_with_budgets(16 * 1024 * 1024, 32 * 1024 * 1024);
        let session = runtime.begin().unwrap();
        let path = root.path().join("encoded.tif");
        std::fs::write(&path, vec![37; 3 * 64 * 1024 + 7]).unwrap();
        let limits = FileLimits {
            edge: 8192,
            pixels: 8192 * 8192,
            encoded_bytes: 128 * 1024 * 1024,
        };
        let disk = runtime.disk().used().unwrap();
        let resident = runtime.resident().used();
        let mut checkpoints = 0;
        {
            let mut source =
                FileSource::open(&session, &path, limits, "tif", &mut || Ok(())).unwrap();
            source
                .snapshot(&mut || {
                    checkpoints += 1;
                    Ok(())
                })
                .unwrap()
                .retire(&mut || session.cleanup_check())
                .unwrap();
        }
        assert!(checkpoints > 3);
        for stop in 1..=checkpoints {
            let mut calls = 0;
            {
                let mut source =
                    FileSource::open(&session, &path, limits, "tif", &mut || Ok(())).unwrap();
                let reason = source
                    .snapshot(&mut || {
                        calls += 1;
                        if calls == stop {
                            Err("Encoded snapshot interrupted".into())
                        } else {
                            Ok(())
                        }
                    })
                    .err()
                    .unwrap();
                assert_eq!(reason, "Encoded snapshot interrupted");
            }
            assert_eq!(runtime.disk().used().unwrap(), disk);
            assert_eq!(runtime.resident().used(), resident);
        }
        assert_eq!(std::fs::read(&path).unwrap(), vec![37; 3 * 64 * 1024 + 7]);
        drop((session, runtime, connection));
    }
}
