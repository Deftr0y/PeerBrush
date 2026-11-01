//! Private linear file mechanics; public native and scalar contracts stay distinct.
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

use super::bytes::ByteRole;
use super::planar::PlanarRole;
#[derive(Clone, Copy)]
pub(super) enum Kind {
    Native(PlanarRole),
    Scalar,
    Bytes(ByteRole, u64),
}
impl Kind {
    fn magic(self) -> &'static [u8; 8] {
        match self {
            Self::Native(_) => b"PBPLAN01",
            Self::Scalar => b"PBF64L01",
            Self::Bytes(..) => b"PBBYTE01",
        }
    }
    fn filename(self) -> &'static str {
        match self {
            Self::Native(_) => "planes.pbp",
            Self::Scalar => "field.pbf",
            Self::Bytes(..) => "bytes.pbb",
        }
    }
    fn role(self) -> u8 {
        match self {
            Self::Native(role) => role as u8,
            Self::Scalar => 1,
            Self::Bytes(role, _) => role as u8,
        }
    }
    fn content_start(self, header: &[u8; 64]) -> Context {
        match self {
            Self::Native(_) => hash_start(b"PBPLANAR-CONTENT1", header),
            Self::Scalar | Self::Bytes(..) => Context::new(&SHA256),
        }
    }
    fn index_domain(self) -> &'static [u8] {
        match self {
            Self::Native(_) => b"PBPLANAR-INDEX1",
            Self::Scalar => b"PBSCALAR-INDEX1",
            Self::Bytes(..) => b"PBBYTES-INDEX1",
        }
    }
    fn seal_domain(self) -> &'static [u8] {
        match self {
            Self::Native(_) => b"PBPLANAR-SEAL1",
            Self::Scalar => b"PBSCALAR-SEAL1",
            Self::Bytes(..) => b"PBBYTES-SEAL1",
        }
    }
}
#[derive(Clone, Copy)]
pub(super) struct Spec {
    pub width: u32,
    pub height: u32,
    pub depth: u16,
    pub channels: u16,
    pub kind: Kind,
}
impl Spec {
    pub fn row_bytes(self) -> usize {
        self.width as usize * (self.depth as usize / 8)
    }
    pub fn payload_bytes(self) -> u64 {
        match self.kind {
            Kind::Bytes(_, bytes) => bytes,
            _ => self.row_bytes() as u64 * self.height as u64 * self.channels as u64,
        }
    }
    pub fn chunks(self) -> usize {
        self.payload_bytes().div_ceil(CHUNK as u64) as usize
    }
    pub fn encoded_bytes(self) -> u64 {
        HEADER + self.payload_bytes() + self.chunks() as u64 * 32 + SEAL as u64
    }
}
pub(super) struct Owned {
    root: PathBuf,
    directory: File,
    directory_id: (u64, u64),
    path: PathBuf,
    pub(super) file: File,
    identity: (u64, u64),
    pub(super) expected: Stamp,
    disk: DiskReservation,
}
impl Owned {
    pub(super) fn create(
        root: &Path,
        filename: &str,
        header: &[u8],
        admitted: u64,
        disk: DiskBudget,
    ) -> Result<Self> {
        if !root.is_absolute() || root.file_name().is_none() || admitted < header.len() as u64 {
            return Err(Error::Invalid(
                "Linear temporary requires a fresh absolute owned namespace",
            ));
        }
        validate_ancestors(root.parent().ok_or(Error::Changed)?)?;
        let charge = disk.reserve(admitted)?;
        let mut builder = fs::DirBuilder::new();
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        builder.create(root)?;
        let directory = open_directory(root)?;
        let directory_id = crate::file_safety::identity(&directory).map_err(|_| Error::Changed)?;
        let path = root.join(filename);
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
        write_all_at(&file, 0, header)?;
        file.sync_all()?;
        crate::server::sync_parent(&path).map_err(|_| Error::Changed)?;
        let owned = Self {
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
        Ok(owned)
    }
    pub(super) fn filesystem_identity(&self) -> ((u64, u64), (u64, u64)) {
        (self.directory_id, self.identity)
    }
    pub(super) fn disk_budget(&self) -> DiskBudget {
        self.disk.budget()
    }
    pub(super) fn grow(&self, bytes: u64) -> Result<()> {
        self.disk.grow(bytes)
    }

    pub(super) fn check(&self) -> Result<()> {
        self.check_identity()?;
        if stamp(&self.file)? != self.expected {
            return Err(Error::Changed);
        }
        Ok(())
    }
    pub(super) fn check_identity(&self) -> Result<()> {
        validate_ancestors(&self.root)?;
        regular(&self.path)?;
        let dir = open_directory(&self.root)?;
        let named =
            crate::file_safety::open_native_identity(&self.path).map_err(|_| Error::Changed)?;
        if crate::file_safety::identity(&dir).map_err(|_| Error::Changed)? != self.directory_id
            || crate::file_safety::identity(&self.directory).map_err(|_| Error::Changed)?
                != self.directory_id
            || crate::file_safety::identity(&named).map_err(|_| Error::Changed)? != self.identity
            || crate::file_safety::identity(&self.file).map_err(|_| Error::Changed)?
                != self.identity
            || links(&self.file)? != 1
        {
            return Err(Error::Changed);
        }
        Ok(())
    }
}

pub(super) struct Scratch {
    pub(super) owned: Owned,
    pub(super) spec: Spec,
    pub(super) budget: ResidentBudget,
    header: [u8; 64],
    pub(super) index: ScratchBytes,
    rows: u64,
    written: u64,
    content: Context,
    chunk: Context,
    chunk_bytes: usize,
    chunk_count: usize,
    poisoned: bool,
}

impl Scratch {
    pub(super) fn filesystem_identity(&self) -> ((u64, u64), (u64, u64)) {
        (self.owned.directory_id, self.owned.identity)
    }
    pub fn uses_budgets(&self, resident: &ResidentBudget, disk: &DiskBudget) -> bool {
        self.budget.same_pool(resident) && self.owned.disk.uses_budget(disk)
    }
    /// The caller retains the actual instance/profile admission for this fresh,
    /// private namespace. Quota and seals never grant that authority.
    pub fn create(
        root: &Path,
        spec: Spec,
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
        let mut header = [0; 64];
        header[..8].copy_from_slice(spec.kind.magic());
        ring::rand::SecureRandom::fill(&ring::rand::SystemRandom::new(), &mut header[8..24])
            .map_err(|_| Error::Invalid("Planar identity creation failed"))?;
        if header[8..24] == [0; 16] {
            return Err(Error::Invalid("Planar identity creation failed"));
        }
        header[24..28].copy_from_slice(&spec.width.to_be_bytes());
        header[28..32].copy_from_slice(&spec.height.to_be_bytes());
        header[32..34].copy_from_slice(&spec.depth.to_be_bytes());
        header[34..36].copy_from_slice(&spec.channels.to_be_bytes());
        header[36] = spec.kind.role();
        header[40..48].copy_from_slice(&spec.payload_bytes().to_be_bytes());
        header[48..56].copy_from_slice(&(spec.chunks() as u64).to_be_bytes());
        header[56..64].copy_from_slice(&(CHUNK as u64).to_be_bytes());
        let owned = Owned::create(
            root,
            spec.kind.filename(),
            &header,
            spec.encoded_bytes(),
            disk,
        )?;
        let content = spec.kind.content_start(&header);
        let chunk = chunk_start(&header, 0);
        Ok(Self {
            owned,
            spec,
            budget,
            header,
            index,
            rows: 0,
            written: 0,
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
        if self.spec.row_bytes() == 0 {
            return Err(Error::Invalid("Byte spools do not accept image rows"));
        }
        let count = bytes.len() / self.spec.row_bytes();
        if channel >= self.spec.channels
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
        self.append_payload(bytes, check)?;
        self.rows += count as u64;
        Ok(())
    }

    pub fn append_bytes(&mut self, bytes: &[u8], check: Check<'_>) -> Result<()> {
        if !matches!(self.spec.kind, Kind::Bytes(..)) {
            return Err(Error::Invalid("Image planes require ordered rows"));
        }
        self.append_payload(bytes, check)
    }

    fn append_payload(&mut self, bytes: &[u8], check: Check<'_>) -> Result<()> {
        if self.poisoned {
            return Err(Error::Interrupted);
        }
        if bytes.len() > 64 * 1024 * 1024
            || self
                .written
                .checked_add(bytes.len() as u64)
                .is_none_or(|end| end > self.spec.payload_bytes())
        {
            return Err(Error::Budget);
        }
        check()?;
        self.owned.check()?;
        let result = (|| {
            let mut at = HEADER + self.written;
            let mut input = bytes;
            while !input.is_empty() {
                check()?;
                let count = input.len().min(CHUNK - self.chunk_bytes);
                let part = &input[..count];
                write_all_at(&self.owned.file, at, part)?;
                self.content.update(part);
                self.chunk.update(part);
                self.written = self
                    .written
                    .checked_add(count as u64)
                    .ok_or(Error::Budget)?;
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
        }
        result
    }

    /// The codec supplies `binding` after its independent source/completion checks.
    /// Sealing proves storage bytes, never permission to edit or recover them.
    pub fn finish(mut self, binding: Digest, check: Check<'_>) -> Result<Arc<Complete>> {
        if self.poisoned {
            return Err(Error::Interrupted);
        }
        if self.rows != self.spec.channels as u64 * self.spec.height as u64
            || self.written != self.spec.payload_bytes()
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
        let mut index_hash = hash_start(self.spec.kind.index_domain(), &self.header);
        index_hash.update(&self.index);
        let index_hash = finish(index_hash);
        let mut seal = [0; SEAL];
        seal[..8].copy_from_slice(b"PBPLSE01");
        seal[8..40].copy_from_slice(&binding);
        seal[40..72].copy_from_slice(&content);
        seal[72..104].copy_from_slice(&index_hash);
        let mut sealed = hash_start(self.spec.kind.seal_domain(), &self.header);
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
        Ok(Arc::new(Complete {
            owned: self.owned,
            spec: self.spec,
            content_sha256: content,
            binding,
            header: self.header,
            index: self.index.freeze(),
            budget: self.budget,
        }))
    }

    /// Never-published writers only. Reconcile only a byte-exact known prefix;
    /// partial unknown writes, changed headers and foreign names remain held.
    pub fn abort(mut self, check: Check<'_>) -> Result<()> {
        check()?;
        self.owned.check_identity()?;
        let before = stamp(&self.owned.file)?;
        if before.bytes != HEADER.checked_add(self.written).ok_or(Error::Budget)? {
            return Err(Error::Changed);
        }
        let expected = finish(self.content);
        let mut buffer = ScratchBytes::new(&self.budget, CHUNK, CHUNK)?;
        read_exact_at(&self.owned.file, 0, &mut buffer[..64])?;
        if &buffer[..64] != &self.header {
            return Err(Error::Corrupt);
        }
        let mut observed = self.spec.kind.content_start(&self.header);
        let mut at = 0;
        while at < self.written {
            check()?;
            let count = (self.written - at).min(CHUNK as u64) as usize;
            read_exact_at(&self.owned.file, HEADER + at, &mut buffer[..count])?;
            observed.update(&buffer[..count]);
            at += count as u64;
        }
        if finish(observed) != expected {
            return Err(Error::Corrupt);
        }
        check()?;
        self.owned.check_identity()?;
        if stamp(&self.owned.file)? != before {
            return Err(Error::Changed);
        }
        self.owned.expected = before;
        retire_owned(self.owned, check)
    }
}

pub(super) struct Complete {
    pub(super) owned: Owned,
    pub(super) spec: Spec,
    pub(super) binding: Digest,
    header: [u8; 64],
    index: PinnedBytes,
    pub(super) content_sha256: Digest,
    pub(super) budget: ResidentBudget,
}
impl Complete {
    pub fn uses_budgets(&self, resident: &ResidentBudget, disk: &DiskBudget) -> bool {
        self.budget.same_pool(resident) && self.owned.disk.uses_budget(disk)
    }
    pub fn read_stripe(
        &self,
        channel: u16,
        y: u32,
        rows: u32,
        check: Check<'_>,
    ) -> Result<ScratchBytes> {
        let bytes = self.stripe_bytes(channel, y, rows)?;
        let mut output = ScratchBytes::new(&self.budget, bytes, 64 * 1024 * 1024)?;
        self.read_into(channel, y, rows, &mut output, check)?;
        Ok(output)
    }
    fn stripe_bytes(&self, channel: u16, y: u32, rows: u32) -> Result<usize> {
        if channel >= self.spec.channels
            || rows == 0
            || rows > 256
            || y.checked_add(rows).is_none_or(|end| end > self.spec.height)
        {
            return Err(Error::Invalid(
                "Planar stripe is outside its admitted shape",
            ));
        }
        let bytes = rows as usize * self.spec.row_bytes();
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
        if output.len() != bytes || !output.uses_budget(&self.budget) {
            return Err(Error::Budget);
        }
        check()?;
        self.owned.check()?;
        let start =
            (channel as u64 * self.spec.height as u64 + y as u64) * self.spec.row_bytes() as u64;
        let end = start + bytes as u64;
        let mut buffer = ScratchBytes::new(&self.budget, CHUNK, CHUNK)?;
        self.copy_range(start, end, output, &mut buffer, &mut None, check)?;
        check()?;
        self.owned.check()
    }

    pub fn read_rect(
        &self,
        x: u32,
        y: u32,
        width: u32,
        height: u32,
        output: &mut ScratchBytes,
        check: Check<'_>,
    ) -> Result<()> {
        if !matches!(self.spec.kind, Kind::Scalar)
            || self.spec.channels != 1
            || self.spec.depth != 64
            || width == 0
            || height == 0
            || height > 256
            || width as u64 * height as u64 > 128 * 1024
            || x.checked_add(width).is_none_or(|end| end > self.spec.width)
            || y.checked_add(height)
                .is_none_or(|end| end > self.spec.height)
        {
            return Err(Error::Invalid(
                "Scalar rectangle exceeds its admitted frame or region budget",
            ));
        }
        let row_bytes = width as usize * 8;
        if output.len() != row_bytes * height as usize || !output.uses_budget(&self.budget) {
            return Err(Error::Budget);
        }
        check()?;
        self.owned.check()?;
        let mut buffer = ScratchBytes::new(&self.budget, CHUNK, CHUNK)?;
        let mut cached = None;
        for row in 0..height {
            check()?;
            let start = (y as u64 + row as u64) * self.spec.row_bytes() as u64 + x as u64 * 8;
            self.copy_range(
                start,
                start + row_bytes as u64,
                &mut output[row as usize * row_bytes..(row as usize + 1) * row_bytes],
                &mut buffer,
                &mut cached,
                check,
            )?;
        }
        check()?;
        self.owned.check()
    }

    pub fn read_range_into(
        &self,
        start: u64,
        output: &mut ScratchBytes,
        check: Check<'_>,
    ) -> Result<()> {
        if output.len() > 64 * 1024 * 1024 || !output.uses_budget(&self.budget) {
            return Err(Error::Budget);
        }
        let mut buffer = ScratchBytes::new(&self.budget, CHUNK, CHUNK)?;
        self.read_bytes(start, output, &mut buffer, check)
    }

    pub fn read_bytes(
        &self,
        start: u64,
        output: &mut [u8],
        buffer: &mut ScratchBytes,
        check: Check<'_>,
    ) -> Result<()> {
        if !matches!(self.spec.kind, Kind::Bytes(..))
            || output.len() > 64 * 1024 * 1024
            || start
                .checked_add(output.len() as u64)
                .is_none_or(|end| end > self.spec.payload_bytes())
        {
            return Err(Error::Invalid("Byte range exceeds its sealed payload"));
        }
        if buffer.len() != CHUNK || !buffer.uses_budget(&self.budget) {
            return Err(Error::Budget);
        }
        check()?;
        self.owned.check()?;
        if output.is_empty() {
            check()?;
            return self.owned.check();
        }
        self.copy_range(
            start,
            start + output.len() as u64,
            output,
            buffer,
            &mut None,
            check,
        )?;
        check()?;
        self.owned.check()
    }

    fn copy_range(
        &self,
        start: u64,
        end: u64,
        output: &mut [u8],
        buffer: &mut ScratchBytes,
        cached: &mut Option<u64>,
        check: Check<'_>,
    ) -> Result<()> {
        for number in start / CHUNK as u64..end.div_ceil(CHUNK as u64) {
            check()?;
            let at = number * CHUNK as u64;
            let count = (self.spec.payload_bytes() - at).min(CHUNK as u64) as usize;
            if *cached != Some(number) {
                read_exact_at(&self.owned.file, HEADER + at, &mut buffer[..count])?;
                let mut hash = chunk_start(&self.header, number as usize);
                hash.update(&buffer[..count]);
                if hash.finish().as_ref()
                    != &self.index[number as usize * 32..number as usize * 32 + 32]
                {
                    return Err(Error::Corrupt);
                }
                *cached = Some(number);
            }
            let left = start.max(at);
            let right = end.min(at + count as u64);
            output[(left - start) as usize..(right - start) as usize]
                .copy_from_slice(&buffer[(left - at) as usize..(right - at) as usize]);
        }
        Ok(())
    }
}

#[cfg(unix)]
pub(super) fn unlink_owned(_file: &File, path: &Path) -> Result<()> {
    // The retained private directory and actual profile writer admission are
    // required; no arbitrary path or disposable-cache deletion is admitted.
    fs::remove_file(path)?;
    Ok(())
}

#[cfg(windows)]
pub(super) fn unlink_owned(file: &File, _path: &Path) -> Result<()> {
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
    let domain: &[u8] = match &header[..8] {
        b"PBF64L01" => b"PBSCALAR-CHUNK1",
        b"PBBYTE01" => b"PBBYTES-CHUNK1",
        _ => b"PBPLANAR-CHUNK1",
    };
    let mut hash = hash_start(domain, header);
    hash.update(&(number as u64).to_be_bytes());
    hash
}
fn finish(hash: Context) -> Digest {
    hash.finish().as_ref().try_into().unwrap()
}

fn verify(
    file: &File,
    spec: Spec,
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
    let mut content = spec.kind.content_start(header);
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
    let mut hash = hash_start(spec.kind.index_domain(), header);
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

/// Explicit retirement only after the codec has released every reader and
/// needs no staged bytes. Failure preserves the quota charge and foreign names.
pub(super) fn retire(owner: Arc<Complete>, check: Check<'_>) -> Result<()> {
    let complete = Arc::try_unwrap(owner).map_err(|_| Error::Busy)?;
    retire_owned(complete.owned, check)
}

pub(super) fn retire_owned(owned: Owned, check: Check<'_>) -> Result<()> {
    check()?;
    owned.check()?;
    check()?;
    owned.check()?;
    unlink_owned(&owned.file, &owned.path)?;
    crate::server::sync_parent(&owned.path).map_err(|_| Error::Changed)?;
    let Owned {
        root,
        directory,
        directory_id,
        path,
        file,
        identity,
        disk,
        ..
    } = owned;
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
