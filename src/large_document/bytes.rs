//! Opaque append-only source-operation spools; no image or publication authority.
use super::{linear, DiskBudget, Error, ResidentBudget, Result, ScratchBytes};
use std::{
    io::{self, Read, Seek, SeekFrom, Write},
    path::Path,
    sync::Arc,
};
type Digest = [u8; 32];
type Check<'a> = &'a mut dyn FnMut() -> Result<()>;
pub const MAX_SOURCE_PAYLOAD: u64 = 64 * 1024 * 1024 * 1024;
const CHUNK: usize = 64 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum ByteRole {
    PhotoshopContainer = 1,
    LayerData = 2,
    SourceBulk = 3,
    CompositeData = 4,
    CompatibleJxlMode1 = 5,
    StraightNativeRgba = 6,
}

pub struct LargeSourceTemporary {
    inner: linear::Scratch,
}
impl LargeSourceTemporary {
    pub(crate) fn filesystem_identity(&self) -> ((u64, u64), (u64, u64)) {
        self.inner.filesystem_identity()
    }
    fn layout(length: u64, role: ByteRole) -> Result<linear::Spec> {
        if length > MAX_SOURCE_PAYLOAD {
            return Err(Error::Budget);
        }
        Ok(linear::Spec {
            width: 0,
            height: 0,
            depth: 0,
            channels: 0,
            kind: linear::Kind::Bytes(role, length),
        })
    }
    pub fn encoded_bytes(length: u64) -> Result<u64> {
        Ok(Self::layout(length, ByteRole::PhotoshopContainer)?.encoded_bytes())
    }
    /// Zero-length payloads are valid opaque blobs; they grant no codec authority.
    pub fn create(
        root: &Path,
        length: u64,
        role: ByteRole,
        resident: ResidentBudget,
        disk: DiskBudget,
    ) -> Result<Self> {
        Ok(Self {
            inner: linear::Scratch::create(root, Self::layout(length, role)?, resident, disk)?,
        })
    }
    pub(crate) fn role(&self) -> ByteRole {
        match self.inner.spec.kind {
            linear::Kind::Bytes(role, _) => role,
            _ => unreachable!("Byte temporary contains an image"),
        }
    }
    pub fn payload_length(&self) -> u64 {
        self.inner.spec.payload_bytes()
    }
    pub fn uses_budgets(&self, resident: &ResidentBudget, disk: &DiskBudget) -> bool {
        self.inner.uses_budgets(resident, disk)
    }
    pub fn append(&mut self, bytes: &[u8], check: Check<'_>) -> Result<()> {
        self.inner.append_bytes(bytes, check)
    }
    pub fn writer<'a>(&'a mut self, check: Check<'a>) -> AppendWriter<'a> {
        AppendWriter {
            temporary: self,
            check,
        }
    }
    pub fn finish(self, binding: Digest, check: Check<'_>) -> Result<CompletedBytes> {
        Ok(CompletedBytes {
            inner: self.inner.finish(binding, check)?,
        })
    }
    pub fn abort(self, check: Check<'_>) -> Result<()> {
        self.inner.abort(check)
    }
}

pub struct AppendWriter<'a> {
    temporary: &'a mut LargeSourceTemporary,
    check: Check<'a>,
}
impl Write for AppendWriter<'_> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.is_empty() {
            return Ok(0);
        }
        let count = bytes.len().min(CHUNK);
        self.temporary
            .append(&bytes[..count], self.check)
            .map_err(io_error)?;
        Ok(count)
    }
    fn flush(&mut self) -> io::Result<()> {
        (self.check)().map_err(io_error)
    }
}

#[derive(Clone)]
pub struct CompletedBytes {
    inner: Arc<linear::Complete>,
}
impl CompletedBytes {
    #[cfg(test)]
    pub(crate) fn test_corrupt_payload(
        &self,
        offset: u64,
        bytes: &[u8],
        restore_modified: bool,
    ) -> Result<()> {
        self.inner
            .test_corrupt_payload(offset, bytes, restore_modified)
    }
    /// Observation after finish/readback; each actual range still verifies its
    /// immutable chunk hash. This is not a fresh whole-file integrity proof.
    pub(crate) fn check_owner(&self) -> Result<()> {
        self.inner.owned.check()
    }

    pub fn payload_length(&self) -> u64 {
        self.inner.spec.payload_bytes()
    }
    pub fn payload_sha256(&self) -> Digest {
        self.inner.content_sha256
    }
    pub fn binding(&self) -> Digest {
        self.inner.binding
    }
    pub fn role(&self) -> ByteRole {
        match self.inner.spec.kind {
            linear::Kind::Bytes(role, _) => role,
            _ => unreachable!("Opaque byte wrapper contains an image"),
        }
    }
    pub fn uses_budgets(&self, resident: &ResidentBudget, disk: &DiskBudget) -> bool {
        self.inner.uses_budgets(resident, disk)
    }
    /// Exact payload bytes read by ONE uncached read_range_into/read_bytes
    /// invocation. Owner checks use metadata only, not header/index/seal reads.
    /// Cursor callers must meter each actual Read separately: adjacent calls
    /// can read the same complete chunk again. This grants no I/O authority.
    pub(crate) fn authenticated_read_cost(&self, start: u64, length: u64) -> Result<u64> {
        let end = start
            .checked_add(length)
            .filter(|end| *end <= self.payload_length())
            .ok_or(Error::Invalid("Byte range exceeds its sealed payload"))?;
        if length > 64 * 1024 * 1024 {
            return Err(Error::Invalid("Byte range exceeds its sealed payload"));
        }
        if length == 0 {
            return Ok(0);
        }
        let first = start / (CHUNK as u64) * (CHUNK as u64);
        let last = (end.div_ceil(CHUNK as u64) * (CHUNK as u64)).min(self.payload_length());
        Ok(last - first)
    }
    pub fn read_range_into(
        &self,
        start: u64,
        output: &mut ScratchBytes,
        check: Check<'_>,
    ) -> Result<()> {
        self.inner.read_range_into(start, output, check)
    }
    /// Each cursor pins its own charged chunk buffer. The caller must separately
    /// admit the transfer buffer supplied to Read; positions are range-relative.
    pub fn cursor<'a>(&self, start: u64, length: u64, check: Check<'a>) -> Result<BytesCursor<'a>> {
        if start
            .checked_add(length)
            .is_none_or(|end| end > self.payload_length())
        {
            return Err(Error::Invalid("Cursor range exceeds its sealed payload"));
        }
        check()?;
        self.inner.owned.check()?;
        let buffer = ScratchBytes::new(&self.inner.budget, CHUNK, CHUNK)?;
        Ok(BytesCursor {
            owner: self.clone(),
            start,
            length,
            position: 0,
            buffer,
            check,
        })
    }
    pub fn retire(self, check: Check<'_>) -> Result<()> {
        linear::retire(self.inner, check)
    }
}

pub struct BytesCursor<'a> {
    owner: CompletedBytes,
    start: u64,
    length: u64,
    position: u64,
    buffer: ScratchBytes,
    check: Check<'a>,
}
impl Read for BytesCursor<'_> {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        if output.is_empty() {
            return Ok(0);
        }
        if self.position == self.length {
            (self.check)().map_err(io_error)?;
            self.owner.inner.owned.check().map_err(io_error)?;
            return Ok(0);
        }
        let count = (self.length - self.position).min(output.len().min(CHUNK) as u64) as usize;
        self.owner
            .inner
            .read_bytes(
                self.start + self.position,
                &mut output[..count],
                &mut self.buffer,
                self.check,
            )
            .map_err(io_error)?;
        self.position += count as u64;
        Ok(count)
    }
}
impl Seek for BytesCursor<'_> {
    fn seek(&mut self, from: SeekFrom) -> io::Result<u64> {
        (self.check)().map_err(io_error)?;
        let position = match from {
            SeekFrom::Start(at) => i128::from(at),
            SeekFrom::End(offset) => i128::from(self.length) + i128::from(offset),
            SeekFrom::Current(offset) => i128::from(self.position) + i128::from(offset),
        };
        if position < 0 || position > i128::from(self.length) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Seek exceeds the sealed cursor range",
            ));
        }
        self.position = position as u64;
        Ok(self.position)
    }
}

fn io_error(error: Error) -> io::Error {
    match error {
        Error::Io(error) => error,
        // Read/Write helpers automatically retry Interrupted. Cancellation is
        // permanent for this operation and must propagate instead of spinning.
        Error::Cancelled => io::Error::other(error),
        Error::Busy => io::Error::new(io::ErrorKind::WouldBlock, error),
        _ => io::Error::new(io::ErrorKind::InvalidData, error),
    }
}

#[cfg(test)]
mod tests {
    use super::super::store::{open_directory, stamp, validate_ancestors, write_all_at};
    use super::*;
    use ring::digest::{Context, SHA256};
    use std::{
        fs::{self, File},
        path::PathBuf,
    };
    struct Fixture {
        root: PathBuf,
        held: Option<File>,
        identity: (u64, u64),
    }
    impl Fixture {
        fn new() -> Self {
            let root =
                std::env::temp_dir().join(format!("peerbrush-byte-tests-{}", uuid::Uuid::new_v4()));
            fs::create_dir(&root).unwrap();
            let root = fs::canonicalize(root).unwrap();
            let held = open_directory(&root).unwrap();
            let identity = crate::file_safety::identity(&held).unwrap();
            Self {
                root,
                held: Some(held),
                identity,
            }
        }
        fn spool(&self) -> PathBuf {
            self.root.join("spool")
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let temp = fs::canonicalize(std::env::temp_dir()).unwrap();
            if self.root.starts_with(temp)
                && self
                    .root
                    .file_name()
                    .is_some_and(|name| name.to_string_lossy().starts_with("peerbrush-byte-tests-"))
                && validate_ancestors(&self.root).is_ok()
                && fs::canonicalize(&self.root).is_ok_and(|path| path == self.root)
                && open_directory(&self.root)
                    .and_then(|file| {
                        crate::file_safety::identity(&file).map_err(|_| Error::Changed)
                    })
                    .is_ok_and(|id| id == self.identity)
                && self.held.as_ref().is_some_and(|file| {
                    crate::file_safety::identity(file).is_ok_and(|id| id == self.identity)
                })
            {
                drop(self.held.take());
                let _ = fs::remove_dir_all(&self.root);
            }
        }
    }
    fn okay() -> Result<()> {
        Ok(())
    }
    fn value(index: u64) -> u8 {
        index.wrapping_mul(37).wrapping_add(11) as u8
    }
    fn filled(
        fixture: &Fixture,
        length: u64,
        budget: &ResidentBudget,
        disk: &DiskBudget,
    ) -> CompletedBytes {
        let mut spool = LargeSourceTemporary::create(
            &fixture.spool(),
            length,
            ByteRole::PhotoshopContainer,
            budget.clone(),
            disk.clone(),
        )
        .unwrap();
        let mut bytes = ScratchBytes::new(budget, CHUNK, CHUNK).unwrap();
        let mut hash = Context::new(&SHA256);
        let mut at = 0;
        while at < length {
            let count = (length - at).min(CHUNK as u64) as usize;
            for (offset, byte) in bytes[..count].iter_mut().enumerate() {
                *byte = value(at + offset as u64);
            }
            hash.update(&bytes[..count]);
            spool.writer(&mut okay).write_all(&bytes[..count]).unwrap();
            at += count as u64;
        }
        let done = spool.finish([29; 32], &mut okay).unwrap();
        assert_eq!(done.payload_sha256(), hash.finish().as_ref());
        done
    }

    #[test]
    fn opaque_spool_ranges_cursors_and_seek_are_exact_and_globally_charged() {
        let fixture = Fixture::new();
        let length = 200123;
        let budget = ResidentBudget::new(1024 * 1024).unwrap();
        let disk =
            DiskBudget::new(LargeSourceTemporary::encoded_bytes(length).unwrap(), 1).unwrap();
        let done = filled(&fixture, length, &budget, &disk);
        assert_eq!(done.payload_length(), length);
        assert_eq!(done.role(), ByteRole::PhotoshopContainer);
        assert_eq!(done.binding(), [29; 32]);
        assert!(done.uses_budgets(&budget, &disk));
        let mut range = ScratchBytes::new(&budget, 10000, 10000).unwrap();
        done.read_range_into(65000, &mut range, &mut okay).unwrap();
        for (offset, byte) in range.iter().enumerate() {
            assert_eq!(*byte, value(65000 + offset as u64));
        }
        drop(range);
        let before = budget.used();
        let mut check = okay;
        let mut cursor = done.cursor(13, 100000, &mut check).unwrap();
        assert_eq!(budget.used(), before + CHUNK);
        assert!(matches!(done.clone().retire(&mut okay), Err(Error::Busy)));
        let mut transfer = ScratchBytes::new(&budget, CHUNK, CHUNK).unwrap();
        cursor.read_exact(&mut transfer[..19]).unwrap();
        for n in 0..19 {
            assert_eq!(transfer[n], value(13 + n as u64));
        }
        assert_eq!(cursor.seek(SeekFrom::End(-16)).unwrap(), 99984);
        cursor.read_exact(&mut transfer[..16]).unwrap();
        for n in 0..16 {
            assert_eq!(transfer[n], value(13 + 99984 + n as u64));
        }
        assert_eq!(cursor.read(&mut transfer).unwrap(), 0);
        assert!(cursor.seek(SeekFrom::Start(100001)).is_err());
        assert_eq!(cursor.seek(SeekFrom::Start(0)).unwrap(), 0);
        cursor.read_exact(&mut transfer[..8]).unwrap();
        drop(cursor);
        drop(transfer);
        assert_eq!(budget.used(), before);
        let mut first = okay;
        let mut second = okay;
        let mut third = okay;
        let a = done.cursor(0, 1, &mut first).unwrap();
        let b = done.cursor(1, 1, &mut second).unwrap();
        let c = done.cursor(2, 1, &mut third).unwrap();
        assert_eq!(budget.used(), before + 3 * CHUNK);
        drop((a, b, c));
        done.retire(&mut okay).unwrap();
        assert_eq!(budget.used(), 0);
        assert_eq!(disk.used().unwrap(), 0);
    }

    #[test]
    fn byte_spool_maximum_and_empty_policy_do_not_raise_legacy_or_image_limits() {
        assert!(LargeSourceTemporary::encoded_bytes(MAX_SOURCE_PAYLOAD).is_ok());
        assert!(matches!(
            LargeSourceTemporary::encoded_bytes(MAX_SOURCE_PAYLOAD + 1),
            Err(Error::Budget)
        ));
        assert!(LargeSourceTemporary::encoded_bytes(u64::MAX).is_err());
        let fixture = Fixture::new();
        let budget = ResidentBudget::new(1024 * 1024).unwrap();
        let disk = DiskBudget::new(208, 1).unwrap();
        let done = filled(&fixture, 0, &budget, &disk);
        assert_eq!(done.payload_length(), 0);
        let mut check = okay;
        let mut cursor = done.cursor(0, 0, &mut check).unwrap();
        assert_eq!(cursor.read(&mut [0; 1]).unwrap(), 0);
        drop(cursor);
        done.retire(&mut okay).unwrap();
        assert_eq!(budget.used(), 0);
        assert_eq!(disk.used().unwrap(), 0);
    }

    #[test]
    fn byte_spool_rejects_wrong_pool_bad_ranges_and_incomplete_publication() {
        let fixture = Fixture::new();
        let budget = ResidentBudget::new(1024 * 1024).unwrap();
        let disk = DiskBudget::new(65536, 2).unwrap();
        let done = filled(&fixture, 1031, &budget, &disk);
        let mut foreign = ScratchBytes::new(&ResidentBudget::new(1024).unwrap(), 8, 8).unwrap();
        assert!(matches!(
            done.read_range_into(0, &mut foreign, &mut okay),
            Err(Error::Budget)
        ));
        let mut output = ScratchBytes::new(&budget, 8, 8).unwrap();
        assert!(done.read_range_into(1030, &mut output, &mut okay).is_err());
        assert!(done.cursor(u64::MAX, 1, &mut okay).is_err());
        drop(output);
        done.retire(&mut okay).unwrap();
        let other = fixture.root.join("incomplete");
        let mut spool = LargeSourceTemporary::create(
            &other,
            100,
            ByteRole::LayerData,
            budget.clone(),
            disk.clone(),
        )
        .unwrap();
        assert!(matches!(
            spool.append(&[0; 101], &mut okay),
            Err(Error::Budget)
        ));
        spool.append(b"short", &mut okay).unwrap();
        assert!(spool.finish([19; 32], &mut okay).is_err());
        assert!(other.join("bytes.pbb").exists());
        assert!(disk.used().unwrap() > 0);
        assert_eq!(budget.used(), 0);
    }

    #[test]
    fn cancellation_propagates_without_read_exact_or_write_all_retry_loops() {
        let fixture = Fixture::new();
        let budget = ResidentBudget::new(1024 * 1024).unwrap();
        let disk = DiskBudget::new(65536, 2).unwrap();
        let done = filled(&fixture, 100, &budget, &disk);
        let mut calls = 0;
        let mut check = || {
            calls += 1;
            if calls == 1 {
                Ok(())
            } else if calls == 2 {
                Err(Error::Cancelled)
            } else {
                Err(Error::Corrupt)
            }
        };
        let mut cursor = done.cursor(0, 10, &mut check).unwrap();
        let error = cursor.read_exact(&mut [0; 8]).unwrap_err();
        assert_ne!(error.kind(), io::ErrorKind::Interrupted);
        drop(cursor);
        assert_eq!(calls, 2);
        done.retire(&mut okay).unwrap();
        let path = fixture.root.join("canceled");
        let mut spool = LargeSourceTemporary::create(
            &path,
            100,
            ByteRole::SourceBulk,
            budget.clone(),
            disk.clone(),
        )
        .unwrap();
        let mut calls = 0;
        let mut check = || {
            calls += 1;
            if calls == 1 {
                Err(Error::Cancelled)
            } else {
                Err(Error::Corrupt)
            }
        };
        let error = spool.writer(&mut check).write_all(b"payload").unwrap_err();
        assert_ne!(error.kind(), io::ErrorKind::Interrupted);
        assert_eq!(calls, 1);
        spool.abort(&mut okay).unwrap();
        assert_eq!(disk.used().unwrap(), 0);
        assert_eq!(budget.used(), 0);
    }

    #[test]
    fn corrupted_or_foreign_spool_reads_never_masquerade_as_eof() {
        for foreign in [false, true] {
            let fixture = Fixture::new();
            let budget = ResidentBudget::new(1024 * 1024).unwrap();
            let disk = DiskBudget::new(65536, 1).unwrap();
            let mut done = filled(&fixture, 1031, &budget, &disk);
            let path = fixture.spool().join("bytes.pbb");
            if foreign {
                fs::rename(&path, fixture.root.join("original")).unwrap();
                fs::write(&path, b"foreign spool").unwrap();
            } else {
                let owned = &mut Arc::get_mut(&mut done.inner).unwrap().owned;
                write_all_at(&owned.file, 64, b"bad").unwrap();
                owned.expected = stamp(&owned.file).unwrap();
            }
            let mut check = okay;
            let cursor = done.cursor(0, 10, &mut check);
            if foreign {
                assert!(cursor.is_err());
                assert!(done.retire(&mut okay).is_err());
                assert_eq!(fs::read(path).unwrap(), b"foreign spool");
            } else {
                let mut cursor = cursor.unwrap();
                assert!(cursor.read(&mut [0; 8]).is_err());
                drop(cursor);
                drop(done);
            }
            assert_eq!(budget.used(), 0);
            assert!(disk.used().unwrap() > 0);
        }
    }
    #[test]
    fn authenticated_range_cost_counts_full_chunks_and_separate_uncached_calls() {
        let f = Fixture::new();
        let budget = ResidentBudget::new(1024 * 1024).unwrap();
        let length = 200123;
        let disk =
            DiskBudget::new(LargeSourceTemporary::encoded_bytes(length).unwrap(), 1).unwrap();
        let done = filled(&f, length, &budget, &disk);
        for (start, count, expected) in [
            (0, 0, 0),
            (1, 0, 0),
            (length, 0, 0),
            (0, 1, 65536),
            (65535, 1, 65536),
            (65535, 2, 131072),
            (65536, 1, 65536),
            (200000, 123, 3515),
            (196607, 2, 69051),
        ] {
            assert_eq!(
                done.authenticated_read_cost(start, count).unwrap(),
                expected
            );
        }
        let separate = done.authenticated_read_cost(65535, 2).unwrap()
            + done.authenticated_read_cost(65537, 2).unwrap();
        assert_eq!(separate, 196608);
        assert_eq!(done.authenticated_read_cost(65535, 4).unwrap(), 131072);
        assert!(done.authenticated_read_cost(length + 1, 0).is_err());
        assert!(done.authenticated_read_cost(u64::MAX, 2).is_err());
        assert!(done
            .authenticated_read_cost(0, 64 * 1024 * 1024 + 1)
            .is_err());
        let mut empty = ScratchBytes::new(&budget, 0, CHUNK).unwrap();
        let before = budget.used();
        done.read_range_into(1, &mut empty, &mut okay).unwrap();
        assert_eq!(budget.used(), before);
        drop(empty);
        done.retire(&mut okay).unwrap();
        assert_eq!(budget.used(), 0);
        assert_eq!(disk.used().unwrap(), 0);
    }
}
