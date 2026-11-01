//! Owned linear canonical channel planes. Completion is storage integrity only.
use super::store::{
    links, open_directory, read_exact_at, regular, source_options, stamp, validate_ancestors,
    write_all_at, Stamp,
};
use super::{
    DiskBudget, DiskReservation, Error, PinnedBytes, ResidentBudget, Result, ScratchBytes,
};
use ring::digest::{Context, SHA256};
use std::{
    fs::{self, File},
    path::{Path, PathBuf},
    sync::Arc,
};

const CHUNK: usize = 64 * 1024;
const HEADER: u64 = 64;
const SEAL: usize = 144;
type Digest = [u8; 32];
type Check<'a> = &'a mut dyn FnMut() -> Result<()>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PlanarSpec {
    width: u32,
    height: u32,
    depth: u16,
}
impl PlanarSpec {
    pub fn new(width: u32, height: u32, depth: u16) -> Result<Self> {
        if width == 0 || height == 0 || width > 16000 || height > 16000 || ![8, 16].contains(&depth)
        {
            return Err(Error::Invalid(
                "Invalid explicitly admitted planar geometry",
            ));
        }
        Ok(Self {
            width,
            height,
            depth,
        })
    }
    pub fn width(self) -> u32 {
        self.width
    }
    pub fn height(self) -> u32 {
        self.height
    }
    pub fn depth(self) -> u16 {
        self.depth
    }
    pub fn row_bytes(self) -> usize {
        self.width as usize * (self.depth as usize / 8)
    }
    pub fn payload_bytes(self) -> u64 {
        self.row_bytes() as u64 * self.height as u64 * 4
    }
    fn chunks(self) -> usize {
        self.payload_bytes().div_ceil(CHUNK as u64) as usize
    }
    pub fn encoded_bytes(self) -> u64 {
        HEADER + self.payload_bytes() + self.chunks() as u64 * 32 + SEAL as u64
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum PlanarRole {
    ImportWhiteMatte = 1,
    SaveCurrentMatte = 2,
}

struct Owned {
    root: PathBuf,
    directory: File,
    directory_id: (u64, u64),
    path: PathBuf,
    file: File,
    identity: (u64, u64),
    expected: Stamp,
    disk: DiskReservation,
}
impl Owned {
    fn check(&self) -> Result<()> {
        validate_ancestors(&self.root)?;
        regular(&self.path)?;
        let dir = open_directory(&self.root)?;
        let named =
            crate::file_safety::open_for_identity(&self.path).map_err(|_| Error::Changed)?;
        if crate::file_safety::identity(&dir).map_err(|_| Error::Changed)? != self.directory_id
            || crate::file_safety::identity(&self.directory).map_err(|_| Error::Changed)?
                != self.directory_id
            || crate::file_safety::identity(&named).map_err(|_| Error::Changed)? != self.identity
            || crate::file_safety::identity(&self.file).map_err(|_| Error::Changed)?
                != self.identity
            || links(&self.file)? != 1
            || stamp(&self.file)? != self.expected
        {
            return Err(Error::Changed);
        }
        Ok(())
    }
}

pub struct PlanarScratch {
    owned: Owned,
    spec: PlanarSpec,
    role: PlanarRole,
    budget: ResidentBudget,
    header: [u8; 64],
    index: ScratchBytes,
    rows: u64,
    content: Context,
    chunk: Context,
    chunk_bytes: usize,
    chunk_count: usize,
    poisoned: bool,
}

impl PlanarScratch {
    pub fn uses_budgets(&self, resident: &ResidentBudget, disk: &DiskBudget) -> bool {
        self.budget.same_pool(resident) && self.owned.disk.uses_budget(disk)
    }
    /// The caller retains the actual instance/profile admission for this fresh,
    /// private namespace. Quota and seals never grant that authority.
    pub fn create(
        root: &Path,
        spec: PlanarSpec,
        role: PlanarRole,
        budget: ResidentBudget,
        disk: DiskBudget,
    ) -> Result<Self> {
        if !root.is_absolute() || root.file_name().is_none() {
            return Err(Error::Invalid(
                "Planar scratch requires an absolute fresh owned directory",
            ));
        }
        validate_ancestors(root.parent().ok_or(Error::Changed)?)?;
        let index = ScratchBytes::new(&budget, spec.chunks() * 32, 64 * 1024 * 1024)?;
        let charge = disk.reserve(spec.encoded_bytes())?;
        let mut builder = fs::DirBuilder::new();
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        builder.create(root)?;
        let directory = open_directory(root)?;
        let directory_id = crate::file_safety::identity(&directory).map_err(|_| Error::Changed)?;
        let path = root.join("planes.pbp");
        let mut options = source_options();
        options.create_new(true);
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            // DELETE is needed to retire this owned handle without unlinking a
            // foreign replacement at the original name.
            options.access_mode(0xC0010000);
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let file = options.open(&path)?;
        file.try_lock().map_err(|_| Error::Busy)?;
        charge.bind(&file)?;
        let identity = crate::file_safety::identity(&file).map_err(|_| Error::Changed)?;
        let mut header = [0; 64];
        header[..8].copy_from_slice(b"PBPLAN01");
        ring::rand::SecureRandom::fill(&ring::rand::SystemRandom::new(), &mut header[8..24])
            .map_err(|_| Error::Invalid("Planar identity creation failed"))?;
        if header[8..24] == [0; 16] {
            return Err(Error::Invalid("Planar identity creation failed"));
        }
        header[24..28].copy_from_slice(&spec.width.to_be_bytes());
        header[28..32].copy_from_slice(&spec.height.to_be_bytes());
        header[32..34].copy_from_slice(&spec.depth.to_be_bytes());
        header[34..36].copy_from_slice(&4u16.to_be_bytes());
        header[36] = role as u8;
        header[40..48].copy_from_slice(&spec.payload_bytes().to_be_bytes());
        header[48..56].copy_from_slice(&(spec.chunks() as u64).to_be_bytes());
        header[56..64].copy_from_slice(&(CHUNK as u64).to_be_bytes());
        write_all_at(&file, 0, &header)?;
        file.sync_all()?;
        crate::server::sync_parent(&path).map_err(|_| Error::Changed)?;
        let owned = Owned {
            root: root.into(),
            directory,
            directory_id,
            path,
            identity,
            expected: stamp(&file)?,
            file,
            disk: charge,
        };
        owned.check()?;
        let content = hash_start(b"PBPLANAR-CONTENT1", &header);
        let chunk = chunk_start(&header, 0);
        Ok(Self {
            owned,
            spec,
            role,
            budget,
            header,
            index,
            rows: 0,
            content,
            chunk,
            chunk_bytes: 0,
            chunk_count: 0,
            poisoned: false,
        })
    }

    /// Append the exact next channel/row in canonical native BE, without seeks.
    pub fn append_rows(
        &mut self,
        channel: u16,
        first_y: u32,
        bytes: &[u8],
        check: Check<'_>,
    ) -> Result<()> {
        if self.poisoned {
            return Err(Error::Interrupted);
        }
        let count = bytes.len() / self.spec.row_bytes();
        if channel >= 4
            || first_y >= self.spec.height
            || bytes.is_empty()
            || bytes.len() % self.spec.row_bytes() != 0
            || count > 256
            || first_y as u64 + count as u64 > self.spec.height as u64
            || self.rows != channel as u64 * self.spec.height as u64 + first_y as u64
        {
            return Err(Error::Invalid(
                "Planar rows must be complete, sequential, and bounded",
            ));
        }
        check()?;
        self.owned.check()?;
        let result = (|| {
            let mut at = HEADER + self.rows * self.spec.row_bytes() as u64;
            let mut input = bytes;
            while !input.is_empty() {
                check()?;
                let count = input.len().min(CHUNK - self.chunk_bytes);
                let part = &input[..count];
                write_all_at(&self.owned.file, at, part)?;
                self.content.update(part);
                self.chunk.update(part);
                self.chunk_bytes += count;
                at += count as u64;
                input = &input[count..];
                if self.chunk_bytes == CHUNK || at == HEADER + self.spec.payload_bytes() {
                    let next = chunk_start(&self.header, self.chunk_count + 1);
                    let digest = std::mem::replace(&mut self.chunk, next).finish();
                    self.index[self.chunk_count * 32..self.chunk_count * 32 + 32]
                        .copy_from_slice(digest.as_ref());
                    self.chunk_count += 1;
                    self.chunk_bytes = 0;
                }
            }
            let expected = stamp(&self.owned.file)?;
            if expected.bytes != at {
                return Err(Error::Changed);
            }
            self.owned.expected = expected;
            self.owned.check()?;
            check()?;
            self.owned.check()?;
            Ok(())
        })();
        if result.is_err() {
            self.poisoned = true;
        } else {
            self.rows += count as u64;
        }
        result
    }

    /// The codec supplies `binding` after its independent source/completion checks.
    /// Sealing proves storage bytes, never permission to edit or recover them.
    pub fn finish(mut self, binding: Digest, check: Check<'_>) -> Result<CompletedPlanar> {
        if self.poisoned {
            return Err(Error::Interrupted);
        }
        if self.rows != 4 * self.spec.height as u64
            || self.chunk_count != self.spec.chunks()
            || binding == [0; 32]
        {
            return Err(Error::Invalid(
                "Planar scratch is incomplete or lacks its source binding",
            ));
        }
        check()?;
        self.owned.check()?;
        let content = finish(self.content);
        let mut index_hash = hash_start(b"PBPLANAR-INDEX1", &self.header);
        index_hash.update(&self.index);
        let index_hash = finish(index_hash);
        let mut seal = [0; SEAL];
        seal[..8].copy_from_slice(b"PBPLSE01");
        seal[8..40].copy_from_slice(&binding);
        seal[40..72].copy_from_slice(&content);
        seal[72..104].copy_from_slice(&index_hash);
        let mut sealed = hash_start(b"PBPLANAR-SEAL1", &self.header);
        sealed.update(&seal[..104]);
        seal[104..136].copy_from_slice(&finish(sealed));
        seal[136..].copy_from_slice(b"PBPLEND1");
        let index_at = HEADER + self.spec.payload_bytes();
        for (n, part) in self.index.chunks(CHUNK).enumerate() {
            check()?;
            write_all_at(&self.owned.file, index_at + (n * CHUNK) as u64, part)?;
        }
        check()?;
        write_all_at(&self.owned.file, index_at + self.index.len() as u64, &seal)?;
        self.owned.file.sync_all()?;
        self.owned.expected = stamp(&self.owned.file)?;
        if self.owned.expected.bytes != self.spec.encoded_bytes() {
            return Err(Error::Changed);
        }
        self.owned.check()?;
        verify(
            &self.owned.file,
            self.spec,
            &self.header,
            &self.index,
            &seal,
            &self.budget,
            check,
        )?;
        check()?;
        self.owned.check()?;
        Ok(CompletedPlanar(Arc::new(Complete {
            owned: self.owned,
            spec: self.spec,
            role: self.role,
            binding,
            header: self.header,
            index: self.index.freeze(),
            budget: self.budget,
        })))
    }
}

struct Complete {
    owned: Owned,
    spec: PlanarSpec,
    role: PlanarRole,
    binding: Digest,
    header: [u8; 64],
    index: PinnedBytes,
    budget: ResidentBudget,
}
#[derive(Clone)]
pub struct CompletedPlanar(Arc<Complete>);
impl CompletedPlanar {
    pub fn uses_budgets(&self, resident: &ResidentBudget, disk: &DiskBudget) -> bool {
        self.0.budget.same_pool(resident) && self.0.owned.disk.uses_budget(disk)
    }
    /// Explicit retirement only after the codec has released every reader and
    /// needs no staged bytes. Failure preserves the quota charge and foreign names.
    pub fn retire(self, check: Check<'_>) -> Result<()> {
        let complete = Arc::try_unwrap(self.0).map_err(|_| Error::Busy)?;
        check()?;
        complete.owned.check()?;
        check()?;
        complete.owned.check()?;
        unlink_owned(&complete.owned.file, &complete.owned.path)?;
        crate::server::sync_parent(&complete.owned.path).map_err(|_| Error::Changed)?;
        let Owned {
            root,
            directory,
            directory_id,
            path,
            file,
            identity,
            disk,
            ..
        } = complete.owned;
        drop(file);
        let named_dir = open_directory(&root)?;
        if crate::file_safety::identity(&directory).map_err(|_| Error::Changed)? != directory_id
            || crate::file_safety::identity(&named_dir).map_err(|_| Error::Changed)? != directory_id
            || !matches!(fs::symlink_metadata(path),Err(error) if error.kind()==std::io::ErrorKind::NotFound)
        {
            return Err(Error::Changed);
        }
        disk.release_retired(identity)
    }
    pub fn spec(&self) -> PlanarSpec {
        self.0.spec
    }
    pub fn role(&self) -> PlanarRole {
        self.0.role
    }
    pub fn binding(&self) -> Digest {
        self.0.binding
    }
    pub fn read_stripe(
        &self,
        channel: u16,
        y: u32,
        rows: u32,
        check: Check<'_>,
    ) -> Result<ScratchBytes> {
        let bytes = self.stripe_bytes(channel, y, rows)?;
        let mut output = ScratchBytes::new(&self.0.budget, bytes, 64 * 1024 * 1024)?;
        self.read_into(channel, y, rows, &mut output, check)?;
        Ok(output)
    }
    fn stripe_bytes(&self, channel: u16, y: u32, rows: u32) -> Result<usize> {
        if channel >= 4
            || rows == 0
            || rows > 256
            || y.checked_add(rows)
                .is_none_or(|end| end > self.0.spec.height)
        {
            return Err(Error::Invalid(
                "Planar stripe is outside its admitted shape",
            ));
        }
        let bytes = rows as usize * self.0.spec.row_bytes();
        if bytes > 64 * 1024 * 1024 {
            return Err(Error::Budget);
        }
        Ok(bytes)
    }
    /// Output remains provisional until the whole call succeeds, including its
    /// final cancellation and ownership checks. Failed reads return no stripe.
    pub fn read_into(
        &self,
        channel: u16,
        y: u32,
        rows: u32,
        output: &mut ScratchBytes,
        check: Check<'_>,
    ) -> Result<()> {
        let bytes = self.stripe_bytes(channel, y, rows)?;
        if output.len() != bytes || !output.uses_budget(&self.0.budget) {
            return Err(Error::Budget);
        }
        check()?;
        self.0.owned.check()?;
        let start = (channel as u64 * self.0.spec.height as u64 + y as u64)
            * self.0.spec.row_bytes() as u64;
        let end = start + bytes as u64;
        let mut buffer = ScratchBytes::new(&self.0.budget, CHUNK, CHUNK)?;
        for number in start / CHUNK as u64..end.div_ceil(CHUNK as u64) {
            check()?;
            let at = number * CHUNK as u64;
            let count = (self.0.spec.payload_bytes() - at).min(CHUNK as u64) as usize;
            read_exact_at(&self.0.owned.file, HEADER + at, &mut buffer[..count])?;
            let mut hash = chunk_start(&self.0.header, number as usize);
            hash.update(&buffer[..count]);
            if hash.finish().as_ref()
                != &self.0.index[number as usize * 32..number as usize * 32 + 32]
            {
                return Err(Error::Corrupt);
            }
            let left = start.max(at);
            let right = end.min(at + count as u64);
            output[(left - start) as usize..(right - start) as usize]
                .copy_from_slice(&buffer[(left - at) as usize..(right - at) as usize]);
        }
        check()?;
        self.0.owned.check()
    }
}

#[cfg(unix)]
fn unlink_owned(_file: &File, path: &Path) -> Result<()> {
    // The retained private directory and actual profile writer admission are
    // required; no arbitrary path or disposable-cache deletion is admitted.
    fs::remove_file(path)?;
    Ok(())
}

#[cfg(windows)]
fn unlink_owned(file: &File, _path: &Path) -> Result<()> {
    use std::os::windows::io::AsRawHandle;
    #[link(name = "kernel32")]
    extern "system" {
        fn SetFileInformationByHandle(
            handle: *mut std::ffi::c_void,
            class: u32,
            info: *const std::ffi::c_void,
            bytes: u32,
        ) -> i32;
    }
    // FILE_DISPOSITION_INFO_EX: DELETE | POSIX_SEMANTICS; available since Win10
    // RS1, below the app's 19041 floor. Do not ignore readonly/security failures.
    let flags = 3u32;
    // SAFETY: File owns the live DELETE-capable handle. The one-DWORD structure
    // matches FileDispositionInfoEx and remains initialized for this call.
    if unsafe {
        SetFileInformationByHandle(file.as_raw_handle(), 21, (&flags as *const u32).cast(), 4)
    } == 0
    {
        return Err(std::io::Error::last_os_error().into());
    }
    Ok(())
}

fn hash_start(domain: &[u8], header: &[u8; 64]) -> Context {
    let mut hash = Context::new(&SHA256);
    hash.update(domain);
    hash.update(header);
    hash
}
fn chunk_start(header: &[u8; 64], number: usize) -> Context {
    let mut hash = hash_start(b"PBPLANAR-CHUNK1", header);
    hash.update(&(number as u64).to_be_bytes());
    hash
}
fn finish(hash: Context) -> Digest {
    hash.finish().as_ref().try_into().unwrap()
}

fn verify(
    file: &File,
    spec: PlanarSpec,
    header: &[u8; 64],
    index: &[u8],
    seal: &[u8; SEAL],
    budget: &ResidentBudget,
    check: Check<'_>,
) -> Result<()> {
    let mut buffer = ScratchBytes::new(budget, CHUNK, CHUNK)?;
    read_exact_at(file, 0, &mut buffer[..64])?;
    if &buffer[..64] != header {
        return Err(Error::Corrupt);
    }
    let mut content = hash_start(b"PBPLANAR-CONTENT1", header);
    for n in 0..spec.chunks() {
        check()?;
        let at = n as u64 * CHUNK as u64;
        let count = (spec.payload_bytes() - at).min(CHUNK as u64) as usize;
        read_exact_at(file, HEADER + at, &mut buffer[..count])?;
        content.update(&buffer[..count]);
        let mut chunk = chunk_start(header, n);
        chunk.update(&buffer[..count]);
        if chunk.finish().as_ref() != &index[n * 32..n * 32 + 32] {
            return Err(Error::Corrupt);
        }
    }
    if content.finish().as_ref() != &seal[40..72] {
        return Err(Error::Corrupt);
    }
    let mut hash = hash_start(b"PBPLANAR-INDEX1", header);
    for (n, expected) in index.chunks(CHUNK).enumerate() {
        check()?;
        read_exact_at(
            file,
            HEADER + spec.payload_bytes() + (n * CHUNK) as u64,
            &mut buffer[..expected.len()],
        )?;
        if &buffer[..expected.len()] != expected {
            return Err(Error::Corrupt);
        }
        hash.update(expected);
    }
    if hash.finish().as_ref() != &seal[72..104] {
        return Err(Error::Corrupt);
    }
    read_exact_at(
        file,
        HEADER + spec.payload_bytes() + index.len() as u64,
        &mut buffer[..SEAL],
    )?;
    if &buffer[..SEAL] != seal {
        return Err(Error::Corrupt);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fixture(PathBuf, Option<File>, (u64, u64));
    impl Fixture {
        fn new() -> Self {
            let path = std::env::temp_dir()
                .join(format!("peerbrush-planar-tests-{}", uuid::Uuid::new_v4()));
            fs::create_dir(&path).unwrap();
            let path = fs::canonicalize(path).unwrap();
            let held = open_directory(&path).unwrap();
            let identity = crate::file_safety::identity(&held).unwrap();
            Self(path, Some(held), identity)
        }
        fn root(&self) -> PathBuf {
            self.0.join("scratch")
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let temp = fs::canonicalize(std::env::temp_dir()).unwrap();
            if self.0.starts_with(temp)
                && self.0.file_name().is_some_and(|name| {
                    name.to_string_lossy()
                        .starts_with("peerbrush-planar-tests-")
                })
                && validate_ancestors(&self.0).is_ok()
                && fs::canonicalize(&self.0).is_ok_and(|path| path == self.0)
                && open_directory(&self.0)
                    .and_then(|file| {
                        crate::file_safety::identity(&file).map_err(|_| Error::Changed)
                    })
                    .is_ok_and(|identity| identity == self.2)
                && self.1.as_ref().is_some_and(|file| {
                    crate::file_safety::identity(file).is_ok_and(|identity| identity == self.2)
                })
            {
                drop(self.1.take());
                let _ = fs::remove_dir_all(&self.0);
            }
        }
    }
    fn okay() -> Result<()> {
        Ok(())
    }
    fn rows(
        spec: PlanarSpec,
        channel: u16,
        y: u32,
        count: u32,
        budget: &ResidentBudget,
    ) -> ScratchBytes {
        let mut data =
            ScratchBytes::new(budget, spec.row_bytes() * count as usize, 64 * 1024 * 1024).unwrap();
        for row in 0..count {
            for x in 0..spec.width {
                let value = ((channel as u32 * 997 + y + row) * 193 + x * 7) as u16;
                let at = row as usize * spec.row_bytes() + x as usize * (spec.depth as usize / 8);
                if spec.depth == 16 {
                    data[at..at + 2].copy_from_slice(&value.to_be_bytes());
                } else {
                    data[at] = value as u8;
                }
            }
        }
        data
    }
    fn fill(scratch: &mut PlanarScratch) {
        for channel in 0..4 {
            let mut y = 0;
            while y < scratch.spec.height {
                let count = (scratch.spec.height - y).min(256);
                let data = rows(scratch.spec, channel, y, count, &scratch.budget);
                scratch.append_rows(channel, y, &data, &mut okay).unwrap();
                y += count;
            }
        }
    }

    #[test]
    fn native_planar_stripes_cross_chunks_and_share_one_charged_completion() {
        for depth in [8, 16] {
            let fixture = Fixture::new();
            let budget = ResidentBudget::new(8 * 1024 * 1024).unwrap();
            let spec = PlanarSpec::new(257, 260, depth).unwrap();
            let disk = DiskBudget::new(spec.encoded_bytes(), 2).unwrap();
            let mut scratch = PlanarScratch::create(
                &fixture.root(),
                spec,
                PlanarRole::ImportWhiteMatte,
                budget.clone(),
                disk.clone(),
            )
            .unwrap();
            assert_eq!(budget.used(), spec.chunks() * 32);
            assert!(scratch.uses_budgets(&budget, &disk));
            fill(&mut scratch);
            let completed = scratch.finish([33; 32], &mut okay).unwrap();
            assert_eq!(completed.spec(), spec);
            assert_eq!(completed.role(), PlanarRole::ImportWhiteMatte);
            assert_eq!(completed.binding(), [33; 32]);
            assert!(completed.uses_budgets(&budget, &disk));
            assert!(!completed
                .uses_budgets(&budget, &DiskBudget::new(spec.encoded_bytes(), 2).unwrap()));
            assert_eq!(disk.used().unwrap(), spec.encoded_bytes());
            for channel in 0..4 {
                let actual = completed.read_stripe(channel, 127, 129, &mut okay).unwrap();
                let expected = rows(spec, channel, 127, 129, &budget);
                assert_eq!(&actual[..], &expected[..]);
            }
            let mut foreign = ScratchBytes::new(
                &ResidentBudget::new(1024 * 1024).unwrap(),
                spec.row_bytes(),
                1024 * 1024,
            )
            .unwrap();
            assert!(matches!(
                completed.read_into(0, 0, 1, &mut foreign, &mut okay),
                Err(Error::Budget)
            ));
            assert!(matches!(
                completed.clone().retire(&mut okay),
                Err(Error::Busy)
            ));
            let mut readers = Vec::new();
            for channel in 0..4 {
                let reader = completed.clone();
                readers.push(std::thread::spawn(move || {
                    for _ in 0..4 {
                        let stripe = reader.read_stripe(channel, 4, 256, &mut okay).unwrap();
                        assert_eq!(stripe.len(), 256 * spec.row_bytes());
                    }
                }));
            }
            for reader in readers {
                reader.join().unwrap();
            }
            completed.retire(&mut okay).unwrap();
            assert_eq!(disk.used().unwrap(), 0);
            assert_eq!(budget.used(), 0);
            assert!(!fixture.root().join("planes.pbp").exists());
        }
    }

    #[test]
    fn invalid_row_order_and_incomplete_seals_never_produce_completion() {
        let fixture = Fixture::new();
        let spec = PlanarSpec::new(17, 260, 16).unwrap();
        let budget = ResidentBudget::new(1024 * 1024).unwrap();
        let disk = DiskBudget::new(spec.encoded_bytes(), 1).unwrap();
        let mut scratch = PlanarScratch::create(
            &fixture.root(),
            spec,
            PlanarRole::SaveCurrentMatte,
            budget.clone(),
            disk.clone(),
        )
        .unwrap();
        let one = rows(spec, 0, 0, 1, &budget);
        for (channel, y, bytes) in [
            (1, 0, &one[..]),
            (0, 1, &one[..]),
            (0, 0, &one[..one.len() - 1]),
        ] {
            assert!(scratch.append_rows(channel, y, bytes, &mut okay).is_err());
            assert_eq!(scratch.owned.file.metadata().unwrap().len(), HEADER);
        }
        scratch.append_rows(0, 0, &one, &mut okay).unwrap();
        drop(one);
        assert!(scratch.finish([7; 32], &mut okay).is_err());
        assert_eq!(disk.used().unwrap(), spec.encoded_bytes());
        assert_eq!(budget.used(), 0);
    }

    #[test]
    fn canceled_partial_append_poison_preserves_file_and_disk_charge() {
        let fixture = Fixture::new();
        let spec = PlanarSpec::new(257, 260, 16).unwrap();
        let budget = ResidentBudget::new(1024 * 1024).unwrap();
        let disk = DiskBudget::new(spec.encoded_bytes(), 1).unwrap();
        let mut scratch = PlanarScratch::create(
            &fixture.root(),
            spec,
            PlanarRole::SaveCurrentMatte,
            budget.clone(),
            disk.clone(),
        )
        .unwrap();
        let data = rows(spec, 0, 0, 256, &budget);
        let probe = scratch.owned.file.try_clone().unwrap();
        assert!(matches!(
            scratch.append_rows(0, 0, &data, &mut || {
                if probe.metadata()?.len() > HEADER {
                    Err(Error::Cancelled)
                } else {
                    Ok(())
                }
            }),
            Err(Error::Cancelled)
        ));
        assert!(probe.metadata().unwrap().len() > HEADER);
        assert!(matches!(
            scratch.append_rows(0, 0, &data, &mut okay),
            Err(Error::Interrupted)
        ));
        drop(data);
        drop(scratch);
        drop(probe);
        assert_eq!(budget.used(), 0);
        assert_eq!(disk.used().unwrap(), spec.encoded_bytes());
        assert!(fixture.root().join("planes.pbp").exists());
    }

    #[test]
    fn chunk_corruption_is_rejected_even_after_matching_attribute_observation() {
        let fixture = Fixture::new();
        let spec = PlanarSpec::new(257, 260, 16).unwrap();
        let budget = ResidentBudget::new(1024 * 1024).unwrap();
        let disk = DiskBudget::new(spec.encoded_bytes(), 1).unwrap();
        let mut scratch = PlanarScratch::create(
            &fixture.root(),
            spec,
            PlanarRole::ImportWhiteMatte,
            budget.clone(),
            disk.clone(),
        )
        .unwrap();
        fill(&mut scratch);
        let mut completed = scratch.finish([7; 32], &mut okay).unwrap();
        let owned = &mut Arc::get_mut(&mut completed.0).unwrap().owned;
        write_all_at(&owned.file, HEADER + 17, b"corrupt").unwrap();
        owned.expected = stamp(&owned.file).unwrap();
        let before = budget.used();
        assert!(matches!(
            completed.read_stripe(0, 0, 1, &mut okay),
            Err(Error::Corrupt)
        ));
        assert_eq!(budget.used(), before);
        drop(completed);
        assert_eq!(budget.used(), 0);
        assert_eq!(disk.used().unwrap(), spec.encoded_bytes());
    }

    #[test]
    fn damaged_payload_or_index_cannot_be_sealed() {
        for damage_index in [false, true] {
            let fixture = Fixture::new();
            let spec = PlanarSpec::new(257, 3, 16).unwrap();
            let budget = ResidentBudget::new(1024 * 1024).unwrap();
            let disk = DiskBudget::new(spec.encoded_bytes(), 1).unwrap();
            let mut scratch = PlanarScratch::create(
                &fixture.root(),
                spec,
                PlanarRole::ImportWhiteMatte,
                budget.clone(),
                disk.clone(),
            )
            .unwrap();
            fill(&mut scratch);
            if damage_index {
                scratch.index[0] ^= 1;
            } else {
                write_all_at(&scratch.owned.file, HEADER + 17, b"bad").unwrap();
                scratch.owned.expected = stamp(&scratch.owned.file).unwrap();
            }
            assert!(matches!(
                scratch.finish([7; 32], &mut okay),
                Err(Error::Corrupt)
            ));
            assert_eq!(budget.used(), 0);
            assert_eq!(disk.used().unwrap(), spec.encoded_bytes());
        }
    }

    #[test]
    fn foreign_replacement_is_preserved_on_read_and_explicit_retirement() {
        let fixture = Fixture::new();
        let spec = PlanarSpec::new(17, 3, 8).unwrap();
        let budget = ResidentBudget::new(1024 * 1024).unwrap();
        let disk = DiskBudget::new(spec.encoded_bytes(), 1).unwrap();
        let mut scratch = PlanarScratch::create(
            &fixture.root(),
            spec,
            PlanarRole::SaveCurrentMatte,
            budget.clone(),
            disk.clone(),
        )
        .unwrap();
        fill(&mut scratch);
        let completed = scratch.finish([7; 32], &mut okay).unwrap();
        let path = fixture.root().join("planes.pbp");
        let moved = fixture.0.join("owned-moved");
        fs::rename(&path, &moved).unwrap();
        fs::write(&path, b"foreign replacement").unwrap();
        assert!(completed.read_stripe(0, 0, 1, &mut okay).is_err());
        assert!(completed.retire(&mut okay).is_err());
        assert_eq!(fs::read(&path).unwrap(), b"foreign replacement");
        assert_eq!(fs::metadata(moved).unwrap().len(), spec.encoded_bytes());
        assert_eq!(disk.used().unwrap(), spec.encoded_bytes());
        assert_eq!(budget.used(), 0);
    }

    #[test]
    fn a_complete_but_canceled_seal_does_not_return_a_completion_token() {
        let fixture = Fixture::new();
        let spec = PlanarSpec::new(17, 3, 16).unwrap();
        let budget = ResidentBudget::new(1024 * 1024).unwrap();
        let disk = DiskBudget::new(spec.encoded_bytes(), 1).unwrap();
        let mut scratch = PlanarScratch::create(
            &fixture.root(),
            spec,
            PlanarRole::SaveCurrentMatte,
            budget.clone(),
            disk.clone(),
        )
        .unwrap();
        fill(&mut scratch);
        let file = scratch.owned.file.try_clone().unwrap();
        assert!(matches!(
            scratch.finish([7; 32], &mut || {
                if file.metadata()?.len() == spec.encoded_bytes() {
                    Err(Error::Cancelled)
                } else {
                    Ok(())
                }
            }),
            Err(Error::Cancelled)
        ));
        assert_eq!(file.metadata().unwrap().len(), spec.encoded_bytes());
        drop(file);
        assert_eq!(budget.used(), 0);
        assert_eq!(disk.used().unwrap(), spec.encoded_bytes());
    }

    #[test]
    #[ignore = "Opt-in 16K native8/16 planar component; writes about3GB, not full editor/PSB qualification"]
    fn full_16000_native8_and_native16_planes_use_bounded_shared_buffers_and_retire() {
        for depth in [8, 16] {
            let started = std::time::Instant::now();
            let fixture = Fixture::new();
            let spec = PlanarSpec::new(16000, 16000, depth).unwrap();
            let budget = ResidentBudget::new(12 * 1024 * 1024).unwrap();
            let disk = DiskBudget::new(spec.encoded_bytes(), 1).unwrap();
            let mut data =
                ScratchBytes::new(&budget, spec.row_bytes() * 256, 64 * 1024 * 1024).unwrap();
            let mut scratch = PlanarScratch::create(
                &fixture.root(),
                spec,
                PlanarRole::SaveCurrentMatte,
                budget.clone(),
                disk.clone(),
            )
            .unwrap();
            let value =
                |channel: u16, y: u32, x: u32| ((channel as u32 * 997 + y) * 193 + x * 7) as u16;
            for channel in 0..4 {
                let mut y = 0;
                while y < spec.height {
                    let count = (spec.height - y).min(256);
                    for row in 0..count {
                        for x in 0..spec.width {
                            let sample = value(channel, y + row, x);
                            let at =
                                row as usize * spec.row_bytes() + x as usize * (depth as usize / 8);
                            if depth == 16 {
                                data[at..at + 2].copy_from_slice(&sample.to_be_bytes());
                            } else {
                                data[at] = sample as u8;
                            }
                        }
                    }
                    scratch
                        .append_rows(
                            channel,
                            y,
                            &data[..spec.row_bytes() * count as usize],
                            &mut okay,
                        )
                        .unwrap();
                    y += count;
                }
                println!("16K planar component depth={depth} channel={channel} payload_written={} resident_bytes={}",scratch.owned.file.metadata().unwrap().len()-HEADER,budget.used());
            }
            let completed = scratch.finish([55; 32], &mut okay).unwrap();
            for channel in 0..4 {
                completed
                    .read_into(channel, 15744, 256, &mut data, &mut okay)
                    .unwrap();
                for row in 0..256 {
                    for x in 0..spec.width {
                        let at =
                            row as usize * spec.row_bytes() + x as usize * (depth as usize / 8);
                        let expected = value(channel, 15744 + row, x);
                        if depth == 16 {
                            assert_eq!(&data[at..at + 2], &expected.to_be_bytes());
                        } else {
                            assert_eq!(data[at], expected as u8);
                        }
                    }
                }
            }
            let mut hash = Context::new(&SHA256);
            let mut io = ScratchBytes::new(&budget, CHUNK, CHUNK).unwrap();
            let mut at = 0;
            while at < spec.encoded_bytes() {
                let count = (spec.encoded_bytes() - at).min(CHUNK as u64) as usize;
                read_exact_at(&completed.0.owned.file, at, &mut io[..count]).unwrap();
                hash.update(&io[..count]);
                at += count as u64;
            }
            let sha = hash
                .finish()
                .as_ref()
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>();
            drop(io);
            assert_eq!(disk.used().unwrap(), spec.encoded_bytes());
            let resident = budget.used();
            completed.retire(&mut okay).unwrap();
            assert_eq!(disk.used().unwrap(), 0);
            drop(data);
            assert_eq!(budget.used(), 0);
            println!("16K planar component PASS depth={depth} payload_bytes={} encoded_bytes={} charged_resident_bytes={resident} elapsed_ms={} file_sha256={sha}; no total RAM/editor/PSB/history qualification",spec.payload_bytes(),spec.encoded_bytes(),started.elapsed().as_millis());
        }
    }
}
