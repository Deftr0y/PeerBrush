use std::{
    collections::BTreeMap,
    fs::{self, File, OpenOptions},
    ops::{Deref, DerefMut},
    path::{Path, PathBuf},
    sync::{Arc, Mutex, MutexGuard, Weak},
    time::SystemTime,
};

use ring::digest::{Context, SHA256};

use super::{access::Access, budget::Charge, Error, ResidentBudget, Result};

const FILE_MAGIC: &[u8; 8] = b"PBTILES1";
const SEAL: &[u8; 8] = b"PBSEAL01";
const FILE_HEADER: u64 = 24;
const RECORD_HEADER: u64 = 44;
const REF_BYTES: usize = 48;
const MANIFEST_HEADER: usize = 64;
const CHUNK: usize = 64 * 1024;
type Digest = [u8; 32];
type Check<'a> = &'a mut dyn FnMut() -> Result<()>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TileSpec {
    width: u16,
    height: u16,
    depth: u16,
}

impl TileSpec {
    pub fn new(width: u16, height: u16, depth: u16) -> Result<Self> {
        if width == 0 || height == 0 || width > 256 || height > 256 || ![8, 16].contains(&depth) {
            return Err(Error::Invalid(
                "Native block requires 1..256 RGBA pixels at depth 8 or 16",
            ));
        }
        Ok(Self {
            width,
            height,
            depth,
        })
    }

    pub fn width(self) -> u16 {
        self.width
    }
    pub fn height(self) -> u16 {
        self.height
    }
    pub fn depth(self) -> u16 {
        self.depth
    }
    pub fn bytes(self) -> usize {
        self.width as usize * self.height as usize * 4 * (self.depth as usize / 8)
    }

    fn encode(self) -> [u8; 8] {
        let mut bytes = [0; 8];
        bytes[..2].copy_from_slice(&self.width.to_be_bytes());
        bytes[2..4].copy_from_slice(&self.height.to_be_bytes());
        bytes[4..6].copy_from_slice(&self.depth.to_be_bytes());
        bytes[6..].copy_from_slice(&4u16.to_be_bytes());
        bytes
    }

    fn decode(bytes: &[u8]) -> Result<Self> {
        if bytes.len() != 8 || bytes[6..] != 4u16.to_be_bytes() {
            return Err(Error::Corrupt);
        }
        Self::new(u16_at(bytes, 0), u16_at(bytes, 2), u16_at(bytes, 4)).map_err(|_| Error::Corrupt)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BlockRef {
    store: [u8; 16],
    digest: Digest,
    offset: u64,
    spec: TileSpec,
}

impl BlockRef {
    pub fn digest(self) -> Digest {
        self.digest
    }
    pub fn spec(self) -> TileSpec {
        self.spec
    }
    pub fn bytes(self) -> usize {
        self.spec.bytes()
    }

    fn encode(self) -> [u8; REF_BYTES] {
        let mut bytes = [0; REF_BYTES];
        bytes[..8].copy_from_slice(&self.spec.encode());
        bytes[8..40].copy_from_slice(&self.digest);
        bytes[40..].copy_from_slice(&self.offset.to_be_bytes());
        bytes
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CommitRef {
    store: [u8; 16],
    digest: Digest,
    offset: u64,
    end: u64,
    source: [u8; 16],
    revision: u64,
}

impl CommitRef {
    pub fn digest(self) -> Digest {
        self.digest
    }
    pub fn source(self) -> [u8; 16] {
        self.source
    }
    pub fn revision(self) -> u64 {
        self.revision
    }

    /// Internal recovery selection only; this is never a project-file runtime path.
    pub fn encode(self) -> [u8; 96] {
        let mut bytes = [0; 96];
        bytes[..8].copy_from_slice(b"PBREF001");
        bytes[8..24].copy_from_slice(&self.store);
        bytes[24..56].copy_from_slice(&self.digest);
        bytes[56..64].copy_from_slice(&self.offset.to_be_bytes());
        bytes[64..72].copy_from_slice(&self.end.to_be_bytes());
        bytes[72..88].copy_from_slice(&self.source);
        bytes[88..96].copy_from_slice(&self.revision.to_be_bytes());
        bytes
    }

    pub fn decode(bytes: &[u8]) -> Result<Self> {
        if bytes.len() != 96 || &bytes[..8] != b"PBREF001" {
            return Err(Error::Corrupt);
        }
        let result = Self {
            store: bytes[8..24].try_into().unwrap(),
            digest: bytes[24..56].try_into().unwrap(),
            offset: u64_at(bytes, 56),
            end: u64_at(bytes, 64),
            source: bytes[72..88].try_into().unwrap(),
            revision: u64_at(bytes, 88),
        };
        if result.store == [0; 16]
            || result.source == [0; 16]
            || result.offset < FILE_HEADER
            || result.end <= result.offset
        {
            return Err(Error::Corrupt);
        }
        Ok(result)
    }
}

/// Explicit source-storage admission. These are not application canvas limits.
#[derive(Clone, Copy)]
pub struct Limits {
    pub disk_bytes: u64,
    pub max_blocks: usize,
    pub max_resident_blocks: usize,
    pub manifest_bytes: usize,
}

impl Limits {
    fn validate(self) -> Result<Self> {
        if self.disk_bytes < FILE_HEADER
            || self.max_blocks == 0
            || self.max_blocks > 1_048_576
            || self.max_resident_blocks == 0
            || self.max_resident_blocks > self.max_blocks
            || self.manifest_bytes < MANIFEST_HEADER
            || self.manifest_bytes > 64 * 1024 * 1024
        {
            return Err(Error::Invalid(
                "Native backing requires explicit bounded disk, index and metadata admission",
            ));
        }
        Ok(self)
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct Stamp {
    bytes: u64,
    modified: SystemTime,
}
fn stamp(file: &File) -> Result<Stamp> {
    let metadata = file.metadata()?;
    if !metadata.is_file() {
        return Err(Error::Changed);
    }
    Ok(Stamp {
        bytes: metadata.len(),
        modified: metadata.modified()?,
    })
}

struct Resident {
    // Field drop order matters: release buffer before its budget charge.
    bytes: Box<[u8]>,
    _charge: Charge,
}

#[derive(Clone)]
pub struct PinnedBytes(Arc<Resident>);
impl Deref for PinnedBytes {
    type Target = [u8];
    fn deref(&self) -> &[u8] {
        &self.0.bytes
    }
}
impl PinnedBytes {
    pub fn allocated_bytes(&self) -> usize {
        self.0.bytes.len()
    }
}

/// Explicit caller scratch under the same byte budget as immutable pins.
pub struct ScratchBytes(Resident);

impl ScratchBytes {
    pub fn new(budget: &ResidentBudget, length: usize, maximum: usize) -> Result<Self> {
        if maximum > 64 * 1024 * 1024 || length > maximum {
            return Err(Error::Budget);
        }
        let charge = budget.reserve(length)?;
        let bytes = super::budget::allocate(length)?;
        Ok(Self(Resident {
            bytes,
            _charge: charge,
        }))
    }

    pub fn allocated_bytes(&self) -> usize {
        self.0.bytes.len()
    }

    pub fn freeze(self) -> PinnedBytes {
        PinnedBytes(Arc::new(self.0))
    }
}

impl Deref for ScratchBytes {
    type Target = [u8];
    fn deref(&self) -> &[u8] {
        &self.0.bytes
    }
}
impl DerefMut for ScratchBytes {
    fn deref_mut(&mut self) -> &mut [u8] {
        &mut self.0.bytes
    }
}

#[derive(Clone)]
pub struct PinnedTile {
    spec: TileSpec,
    bytes: PinnedBytes,
}
impl PinnedTile {
    pub fn spec(&self) -> TileSpec {
        self.spec
    }
    /// Native16 samples are canonical big-endian pairs, never host-endian casts.
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }
    pub fn allocated_bytes(&self) -> usize {
        self.bytes.allocated_bytes()
    }
}

struct State {
    expected: Stamp,
    blocks: BTreeMap<Digest, BlockRef>,
    head: Option<CommitRef>,
    readonly: bool,
    poisoned: bool,
}

struct Inner {
    root: PathBuf,
    directory: File,
    directory_id: (u64, u64),
    path: PathBuf,
    file: File,
    identity: (u64, u64),
    id: [u8; 16],
    limits: Limits,
    budget: ResidentBudget,
    access: Access,
    state: Mutex<State>,
    cache: Mutex<BTreeMap<(u8, Digest), Weak<Resident>>>,
}

impl Drop for Inner {
    fn drop(&mut self) {
        // Source backing survives its handles. No file is unlinked on failure/drop.
        let _ = self.file.unlock();
    }
}

#[derive(Clone)]
pub struct Store(Arc<Inner>);

#[derive(Clone)]
pub struct Snapshot {
    store: Store,
    commit: CommitRef,
    metadata: (u64, usize),
    blocks: (u64, usize),
    metadata_digest: Digest,
    refs_digest: Digest,
}

impl Snapshot {
    pub fn store(&self) -> Store {
        self.store.clone()
    }

    pub fn reference(&self) -> CommitRef {
        self.commit
    }
    pub fn has_unconfirmed_tail(&self) -> Result<bool> {
        let state = self.store.state()?;
        Ok(state.readonly || state.poisoned)
    }

    pub fn metadata(&self, check: Check<'_>) -> Result<PinnedBytes> {
        self.store.load(
            (2, self.metadata_digest),
            self.metadata.1,
            check,
            |bytes, check| {
                read_checked(&self.store.0.file, self.metadata.0, bytes, check)?;
                if digest_parts(b"META", [&bytes[..]], check)? != self.metadata_digest {
                    return Err(Error::Corrupt);
                }
                Ok(())
            },
        )
    }

    /// Streams references rather than allocating a second complete manifest table.
    /// Visitor output remains provisional until this method succeeds.
    pub fn visit_blocks(
        &self,
        check: Check<'_>,
        mut visit: impl FnMut(BlockRef) -> Result<()>,
    ) -> Result<()> {
        let _read = self.store.0.access.read()?;
        let expected = self.store.state()?.expected;
        self.store.check_source(expected)?;
        visit_refs(
            &self.store,
            self.blocks.0,
            self.blocks.1,
            self.refs_digest,
            check,
            &mut visit,
        )?;
        check()?;
        self.store.check_source(expected)
    }
}

impl Store {
    pub fn resident_budget(&self) -> ResidentBudget {
        self.0.budget.clone()
    }

    /// Create a fresh private directory below an already-owned profile/session parent.
    pub fn create(root: &Path, budget: ResidentBudget, limits: Limits) -> Result<Self> {
        let limits = limits.validate()?;
        if !root.is_absolute() || root.file_name().is_none() {
            return Err(Error::Invalid(
                "Native backing requires an absolute fresh directory",
            ));
        }
        let parent = root
            .parent()
            .ok_or(Error::Invalid("Native backing parent is missing"))?;
        validate_ancestors(parent)?;
        let mut builder = fs::DirBuilder::new();
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        builder.create(root)?;
        let directory = open_directory(root)?;
        let directory_id = crate::file_safety::identity(&directory).map_err(|_| Error::Changed)?;
        let path = root.join("blocks.pbt");
        let mut options = source_options();
        options.create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let file = options.open(&path)?;
        file.try_lock().map_err(|_| Error::Busy)?;
        let identity = crate::file_safety::identity(&file).map_err(|_| Error::Changed)?;
        let mut id = [0; 16];
        ring::rand::SecureRandom::fill(&ring::rand::SystemRandom::new(), &mut id)
            .map_err(|_| Error::Invalid("Native backing identity could not be created"))?;
        if id == [0; 16] {
            return Err(Error::Invalid(
                "Native backing identity could not be created",
            ));
        }
        write_all_at(&file, 0, FILE_MAGIC)?;
        write_all_at(&file, 8, &id)?;
        file.sync_all()?;
        crate::server::sync_parent(&path).map_err(|_| Error::Changed)?;
        let store = Self(Arc::new(Inner {
            root: root.into(),
            directory,
            directory_id,
            path,
            identity,
            id,
            limits,
            budget,
            access: Access::default(),
            state: Mutex::new(State {
                expected: stamp(&file)?,
                blocks: BTreeMap::new(),
                head: None,
                readonly: false,
                poisoned: false,
            }),
            file,
            cache: Mutex::new(BTreeMap::new()),
        }));
        let expected = store.state()?.expected;
        store.check_source(expected)?;
        Ok(store)
    }

    /// Reopen only an independently retained checkpoint, never an inferred latest file.
    /// Any suffix after that checkpoint is retained and makes the store read-only.
    pub fn recover(
        root: &Path,
        selected: CommitRef,
        budget: ResidentBudget,
        limits: Limits,
        check: Check<'_>,
    ) -> Result<Snapshot> {
        let limits = limits.validate()?;
        if !root.is_absolute() || selected.end > limits.disk_bytes {
            return Err(Error::Budget);
        }
        validate_ancestors(root)?;
        let directory = open_directory(root)?;
        let directory_id = crate::file_safety::identity(&directory).map_err(|_| Error::Changed)?;
        let path = root.join("blocks.pbt");
        regular(&path)?;
        let file = source_options().open(&path)?;
        file.try_lock().map_err(|_| Error::Busy)?;
        let identity = crate::file_safety::identity(&file).map_err(|_| Error::Changed)?;
        let expected = stamp(&file)?;
        if expected.bytes < selected.end {
            return Err(Error::Corrupt);
        }
        let mut header = [0; FILE_HEADER as usize];
        read_exact_at(&file, 0, &mut header)?;
        if &header[..8] != FILE_MAGIC || header[8..] != selected.store {
            return Err(Error::Corrupt);
        }
        let store = Self(Arc::new(Inner {
            root: root.into(),
            directory,
            directory_id,
            path,
            file,
            identity,
            id: selected.store,
            limits,
            budget,
            access: Access::default(),
            state: Mutex::new(State {
                expected,
                blocks: BTreeMap::new(),
                head: None,
                readonly: expected.bytes != selected.end,
                poisoned: false,
            }),
            cache: Mutex::new(BTreeMap::new()),
        }));
        store.check_source(expected)?;
        let mut offset = FILE_HEADER;
        let mut last = None;
        while offset < selected.end {
            check()?;
            let record = read_record(&store.0.file, offset, selected.end, limits, check)?;
            if &record.kind == b"TILE" {
                let block = store.block_record(&record)?;
                let mut state = store.state()?;
                if state.blocks.len() >= limits.max_blocks
                    || state.blocks.insert(block.digest, block).is_some()
                {
                    return Err(Error::Corrupt);
                }
            } else if &record.kind == b"CMIT" {
                let snapshot = store.manifest_record(&record, check)?;
                last = Some(snapshot);
            } else {
                return Err(Error::Corrupt);
            }
            offset = record.end;
        }
        let snapshot = last.ok_or(Error::Corrupt)?;
        if snapshot.commit != selected || offset != selected.end {
            return Err(Error::Corrupt);
        }
        check()?;
        store.check_source(expected)?;
        Ok(snapshot)
    }

    pub fn put(&self, spec: TileSpec, bytes: &[u8], check: Check<'_>) -> Result<BlockRef> {
        if bytes.len() != spec.bytes() {
            return Err(Error::Invalid(
                "Native tile byte length differs from its specification",
            ));
        }
        check()?;
        let _write = self.0.access.write()?;
        self.writable()?;
        let spec_bytes = spec.encode();
        let digest = digest_parts(b"TILE", [&spec_bytes[..], bytes], check)?;
        let expected = self.state()?.expected;
        self.check_source(expected)?;
        if let Some(reference) = self.state()?.blocks.get(&digest).copied() {
            return Ok(reference);
        }
        if self.state()?.blocks.len() >= self.0.limits.max_blocks {
            return Err(Error::Budget);
        }
        let record = self.append(b"TILE", [&spec_bytes[..], bytes], digest, check)?;
        let reference = BlockRef {
            store: self.0.id,
            digest,
            offset: record.offset,
            spec,
        };
        self.state()?.blocks.insert(digest, reference);
        Ok(reference)
    }

    pub fn pin(&self, reference: BlockRef, check: Check<'_>) -> Result<PinnedTile> {
        if reference.store != self.0.id
            || self.state()?.blocks.get(&reference.digest) != Some(&reference)
        {
            return Err(Error::Corrupt);
        }
        let bytes = self.load(
            (1, reference.digest),
            reference.bytes(),
            check,
            |bytes, check| {
                read_checked(
                    &self.0.file,
                    reference.offset + RECORD_HEADER + 8,
                    bytes,
                    check,
                )?;
                let spec = reference.spec.encode();
                if digest_parts(b"TILE", [&spec[..], &bytes[..]], check)? != reference.digest {
                    return Err(Error::Corrupt);
                }
                Ok(())
            },
        )?;
        Ok(PinnedTile {
            spec: reference.spec,
            bytes,
        })
    }

    pub fn commit(
        &self,
        source: [u8; 16],
        revision: u64,
        parent: Option<CommitRef>,
        metadata: &[u8],
        blocks: &[BlockRef],
        check: Check<'_>,
    ) -> Result<Snapshot> {
        if source == [0; 16]
            || metadata.len() > u32::MAX as usize
            || blocks.len() > u32::MAX as usize
        {
            return Err(Error::Invalid(
                "Native checkpoint source or length is invalid",
            ));
        }
        check()?;
        let _write = self.0.access.write()?;
        self.writable()?;
        let state = self.state()?;
        if state.head != parent
            || parent.is_some_and(|p| p.source != source || revision <= p.revision)
        {
            return Err(Error::Stale);
        }
        if blocks.len() > self.0.limits.max_blocks
            || blocks
                .iter()
                .any(|b| b.store != self.0.id || state.blocks.get(&b.digest) != Some(b))
        {
            return Err(Error::Corrupt);
        }
        drop(state);
        let length = MANIFEST_HEADER
            .checked_add(metadata.len())
            .and_then(|n| {
                blocks
                    .len()
                    .checked_mul(REF_BYTES)
                    .and_then(|b| n.checked_add(b))
            })
            .ok_or(Error::Budget)?;
        if length > self.0.limits.manifest_bytes {
            return Err(Error::Budget);
        }
        let mut header = [0; MANIFEST_HEADER];
        header[..16].copy_from_slice(&source);
        header[16..24].copy_from_slice(&revision.to_be_bytes());
        header[24..56].copy_from_slice(&parent.map_or([0; 32], |p| p.digest));
        header[56..60].copy_from_slice(&(metadata.len() as u32).to_be_bytes());
        header[60..64].copy_from_slice(&(blocks.len() as u32).to_be_bytes());
        let mut context = Context::new(&SHA256);
        context.update(b"CMIT");
        context.update(&header);
        for chunk in metadata.chunks(CHUNK) {
            check()?;
            context.update(chunk);
        }
        for block in blocks {
            check()?;
            context.update(&block.encode());
        }
        let digest = context.finish().as_ref().try_into().unwrap();
        let metadata_digest = digest_parts(b"META", [metadata], check)?;
        let mut refs_context = Context::new(&SHA256);
        refs_context.update(b"REFS");
        for block in blocks {
            check()?;
            refs_context.update(&block.encode());
        }
        let refs_digest = refs_context.finish().as_ref().try_into().unwrap();
        // References are emitted from fixed stack buffers, not a second whole table.
        let record = self.append_manifest(&header, metadata, blocks, length, digest, check)?;
        let commit = CommitRef {
            store: self.0.id,
            digest,
            offset: record.offset,
            end: record.end,
            source,
            revision,
        };
        self.state()?.head = Some(commit);
        Ok(Snapshot {
            store: self.clone(),
            commit,
            metadata: (record.body + MANIFEST_HEADER as u64, metadata.len()),
            blocks: (
                record.body + MANIFEST_HEADER as u64 + metadata.len() as u64,
                blocks.len(),
            ),
            metadata_digest,
            refs_digest,
        })
    }

    fn state(&self) -> Result<MutexGuard<'_, State>> {
        self.0.state.lock().map_err(|_| Error::Interrupted)
    }

    fn writable(&self) -> Result<()> {
        let state = self.state()?;
        if state.poisoned || state.readonly {
            return Err(Error::Interrupted);
        }
        let expected = state.expected;
        drop(state);
        self.check_source(expected)
    }

    fn check_source(&self, expected: Stamp) -> Result<()> {
        validate_ancestors(&self.0.root)?;
        regular(&self.0.path)?;
        let named_root = open_directory(&self.0.root)?;
        let named =
            crate::file_safety::open_for_identity(&self.0.path).map_err(|_| Error::Changed)?;
        if crate::file_safety::identity(&self.0.directory).map_err(|_| Error::Changed)?
            != self.0.directory_id
            || crate::file_safety::identity(&named_root).map_err(|_| Error::Changed)?
                != self.0.directory_id
            || crate::file_safety::identity(&self.0.file).map_err(|_| Error::Changed)?
                != self.0.identity
            || crate::file_safety::identity(&named).map_err(|_| Error::Changed)? != self.0.identity
            || links(&self.0.file)? != 1
            || stamp(&self.0.file)? != expected
        {
            return Err(Error::Changed);
        }
        Ok(())
    }

    fn load(
        &self,
        key: (u8, Digest),
        length: usize,
        check: Check<'_>,
        read: impl FnOnce(&mut [u8], Check<'_>) -> Result<()>,
    ) -> Result<PinnedBytes> {
        let _read = self.0.access.read()?;
        let expected = self.state()?.expected;
        self.check_source(expected)?;
        check()?;
        let cached = self
            .0
            .cache
            .lock()
            .map_err(|_| Error::Interrupted)?
            .get(&key)
            .and_then(Weak::upgrade);
        if let Some(data) = cached {
            self.check_source(expected)?;
            return Ok(PinnedBytes(data));
        }
        let charge = self.0.budget.reserve(length)?;
        let mut bytes = super::budget::allocate(length)?;
        read(&mut bytes, check)?;
        check()?;
        self.check_source(expected)?;
        let candidate = Arc::new(Resident {
            bytes,
            _charge: charge,
        });
        let selected = {
            let mut cache = self.0.cache.lock().map_err(|_| Error::Interrupted)?;
            cache.retain(|_, data| data.strong_count() != 0);
            if let Some(existing) = cache.get(&key).and_then(Weak::upgrade) {
                existing
            } else {
                if cache.len() >= self.0.limits.max_resident_blocks {
                    return Err(Error::Budget);
                }
                cache.insert(key, Arc::downgrade(&candidate));
                candidate.clone()
            }
        };
        drop(candidate);
        Ok(PinnedBytes(selected))
    }

    fn block_record(&self, record: &Record) -> Result<BlockRef> {
        let spec = record.spec.ok_or(Error::Corrupt)?;
        if record.length != (8 + spec.bytes()) as u64 {
            return Err(Error::Corrupt);
        }
        Ok(BlockRef {
            store: self.0.id,
            digest: record.digest,
            offset: record.offset,
            spec,
        })
    }

    fn manifest_record(&self, record: &Record, check: Check<'_>) -> Result<Snapshot> {
        if record.length < MANIFEST_HEADER as u64 {
            return Err(Error::Corrupt);
        }
        let bytes = record.manifest.ok_or(Error::Corrupt)?;
        let source: [u8; 16] = bytes[..16].try_into().unwrap();
        let revision = u64_at(&bytes, 16);
        let parent: Digest = bytes[24..56].try_into().unwrap();
        let metadata = u32_at(&bytes, 56) as usize;
        let count = u32_at(&bytes, 60) as usize;
        if source == [0; 16]
            || count > self.0.limits.max_blocks
            || MANIFEST_HEADER
                .checked_add(metadata)
                .and_then(|n| count.checked_mul(REF_BYTES).and_then(|b| n.checked_add(b)))
                .map(|n| n as u64)
                != Some(record.length)
        {
            return Err(Error::Corrupt);
        }
        let head = self.state()?.head;
        if head.map_or([0; 32], |p| p.digest) != parent
            || head.is_some_and(|p| p.source != source || revision <= p.revision)
        {
            return Err(Error::Corrupt);
        }
        let start = record.body + MANIFEST_HEADER as u64 + metadata as u64;
        visit_refs(self, start, count, record.refs_digest, check, &mut |_| {
            Ok(())
        })?;
        let commit = CommitRef {
            store: self.0.id,
            digest: record.digest,
            offset: record.offset,
            end: record.end,
            source,
            revision,
        };
        self.state()?.head = Some(commit);
        Ok(Snapshot {
            store: self.clone(),
            commit,
            metadata: (record.body + MANIFEST_HEADER as u64, metadata),
            blocks: (start, count),
            metadata_digest: record.metadata_digest,
            refs_digest: record.refs_digest,
        })
    }

    fn append<'a>(
        &self,
        kind: &[u8; 4],
        parts: impl IntoIterator<Item = &'a [u8]> + Clone,
        digest: Digest,
        check: Check<'_>,
    ) -> Result<Record> {
        let length = parts.clone().into_iter().try_fold(0u64, |n, bytes| {
            n.checked_add(bytes.len() as u64).ok_or(Error::Budget)
        })?;
        self.append_with(kind, length, digest, check, |offset, check| {
            let mut at = offset;
            for bytes in parts {
                append_bytes(&self.0.file, &mut at, bytes, check)?;
            }
            Ok(at)
        })
    }

    fn append_manifest(
        &self,
        header: &[u8],
        metadata: &[u8],
        blocks: &[BlockRef],
        length: usize,
        digest: Digest,
        check: Check<'_>,
    ) -> Result<Record> {
        self.append_with(b"CMIT", length as u64, digest, check, |offset, check| {
            let mut at = offset;
            append_bytes(&self.0.file, &mut at, header, check)?;
            append_bytes(&self.0.file, &mut at, metadata, check)?;
            for block in blocks {
                append_bytes(&self.0.file, &mut at, &block.encode(), check)?;
            }
            Ok(at)
        })
    }

    fn append_with(
        &self,
        kind: &[u8; 4],
        length: u64,
        digest: Digest,
        check: Check<'_>,
        write: impl FnOnce(u64, Check<'_>) -> Result<u64>,
    ) -> Result<Record> {
        let expected = self.state()?.expected;
        let end = expected
            .bytes
            .checked_add(RECORD_HEADER)
            .and_then(|n| n.checked_add(length))
            .and_then(|n| n.checked_add(SEAL.len() as u64))
            .filter(|n| *n <= self.0.limits.disk_bytes)
            .ok_or(Error::Budget)?;
        check()?;
        self.check_source(expected)?;
        let record = Record {
            kind: *kind,
            length,
            digest,
            offset: expected.bytes,
            body: expected.bytes + RECORD_HEADER,
            end,
            spec: None,
            manifest: None,
            metadata_digest: [0; 32],
            refs_digest: [0; 32],
        };
        let result = (|| {
            let mut header = [0; RECORD_HEADER as usize];
            header[..4].copy_from_slice(kind);
            header[4..12].copy_from_slice(&length.to_be_bytes());
            header[12..].copy_from_slice(&digest);
            write_all_at(&self.0.file, expected.bytes, &header)?;
            let written = write(record.body, check)?;
            if written != end - SEAL.len() as u64 {
                return Err(Error::Corrupt);
            }
            check()?;
            write_all_at(&self.0.file, written, SEAL)?;
            self.0.file.sync_all()?;
            let after = stamp(&self.0.file)?;
            if after.bytes != end {
                return Err(Error::Changed);
            }
            self.check_source(after)?;
            let checked = read_record(&self.0.file, expected.bytes, end, self.0.limits, check)?;
            if checked.digest != digest || checked.kind != *kind {
                return Err(Error::Corrupt);
            }
            check()?;
            self.check_source(after)?;
            self.state()?.expected = after;
            Ok(record)
        })();
        if result.is_err() {
            let after = stamp(&self.0.file);
            let mut state = self.state()?;
            state.poisoned = true;
            if let Ok(after) = after {
                state.expected = after;
            }
        }
        result
    }
}

#[derive(Clone, Copy)]
struct Record {
    kind: [u8; 4],
    length: u64,
    digest: Digest,
    offset: u64,
    body: u64,
    end: u64,
    spec: Option<TileSpec>,
    manifest: Option<[u8; MANIFEST_HEADER]>,
    metadata_digest: Digest,
    refs_digest: Digest,
}

fn read_record(
    file: &File,
    offset: u64,
    boundary: u64,
    limits: Limits,
    check: Check<'_>,
) -> Result<Record> {
    let mut header = [0; RECORD_HEADER as usize];
    read_exact_at(file, offset, &mut header)?;
    let kind: [u8; 4] = header[..4].try_into().unwrap();
    let length = u64_at(&header, 4);
    let maximum = if &kind == b"TILE" {
        8 + 256 * 256 * 8
    } else if &kind == b"CMIT" {
        limits.manifest_bytes
    } else {
        return Err(Error::Corrupt);
    };
    let end = offset
        .checked_add(RECORD_HEADER)
        .and_then(|n| n.checked_add(length))
        .and_then(|n| n.checked_add(SEAL.len() as u64))
        .filter(|n| *n <= boundary)
        .ok_or(Error::Corrupt)?;
    if length > maximum as u64 {
        return Err(Error::Corrupt);
    }
    let digest: Digest = header[12..].try_into().unwrap();
    let body = offset + RECORD_HEADER;
    let mut context = Context::new(&SHA256);
    context.update(&kind);
    let mut metadata_context = Context::new(&SHA256);
    metadata_context.update(b"META");
    let mut refs_context = Context::new(&SHA256);
    refs_context.update(b"REFS");
    let mut spec = None;
    let mut manifest = None;
    let mut metadata_end = 0;
    let mut buffer = [0; CHUNK];
    let mut at = body;
    while at < end - SEAL.len() as u64 {
        check()?;
        let count = (end - SEAL.len() as u64 - at).min(CHUNK as u64) as usize;
        read_exact_at(file, at, &mut buffer[..count])?;
        context.update(&buffer[..count]);
        if at == body {
            if &kind == b"TILE" {
                if count < 8 {
                    return Err(Error::Corrupt);
                }
                let value = TileSpec::decode(&buffer[..8])?;
                if length != (8 + value.bytes()) as u64 {
                    return Err(Error::Corrupt);
                }
                spec = Some(value);
            } else {
                if count < MANIFEST_HEADER {
                    return Err(Error::Corrupt);
                }
                let prefix: [u8; MANIFEST_HEADER] = buffer[..MANIFEST_HEADER].try_into().unwrap();
                let metadata_bytes = u32_at(&prefix, 56) as u64;
                let refs_count = u32_at(&prefix, 60) as u64;
                if refs_count > limits.max_blocks as u64
                    || (MANIFEST_HEADER as u64)
                        .checked_add(metadata_bytes)
                        .and_then(|n| {
                            refs_count
                                .checked_mul(REF_BYTES as u64)
                                .and_then(|r| n.checked_add(r))
                        })
                        != Some(length)
                {
                    return Err(Error::Corrupt);
                }
                metadata_end = MANIFEST_HEADER as u64 + metadata_bytes;
                manifest = Some(prefix);
            }
        }
        if manifest.is_some() {
            let relative = at - body;
            let meta_start = (MANIFEST_HEADER as u64)
                .saturating_sub(relative)
                .min(count as u64) as usize;
            let meta_end = metadata_end.saturating_sub(relative).min(count as u64) as usize;
            if meta_start < meta_end {
                metadata_context.update(&buffer[meta_start..meta_end]);
            }
            let refs_start = metadata_end.saturating_sub(relative).min(count as u64) as usize;
            refs_context.update(&buffer[refs_start..count]);
        }
        at += count as u64;
    }
    let mut seal = [0; 8];
    read_exact_at(file, at, &mut seal)?;
    if &seal != SEAL || context.finish().as_ref() != digest {
        return Err(Error::Corrupt);
    }
    Ok(Record {
        kind,
        length,
        digest,
        offset,
        body,
        end,
        spec,
        manifest,
        metadata_digest: metadata_context.finish().as_ref().try_into().unwrap(),
        refs_digest: refs_context.finish().as_ref().try_into().unwrap(),
    })
}

fn visit_refs(
    store: &Store,
    start: u64,
    count: usize,
    expected: Digest,
    check: Check<'_>,
    visit: &mut impl FnMut(BlockRef) -> Result<()>,
) -> Result<()> {
    let mut context = Context::new(&SHA256);
    context.update(b"REFS");
    let mut buffer = [0; REF_BYTES * 1024];
    let mut index = 0;
    while index < count {
        check()?;
        let batch = (count - index).min(1024);
        read_exact_at(
            &store.0.file,
            start + (index * REF_BYTES) as u64,
            &mut buffer[..batch * REF_BYTES],
        )?;
        context.update(&buffer[..batch * REF_BYTES]);
        for bytes in buffer[..batch * REF_BYTES].chunks_exact(REF_BYTES) {
            let reference = decode_ref(store.0.id, bytes.try_into().unwrap())?;
            if store.state()?.blocks.get(&reference.digest) != Some(&reference) {
                return Err(Error::Corrupt);
            }
            visit(reference)?;
        }
        index += batch;
    }
    check()?;
    if context.finish().as_ref() != expected {
        return Err(Error::Corrupt);
    }
    Ok(())
}

fn decode_ref(store: [u8; 16], bytes: &[u8; REF_BYTES]) -> Result<BlockRef> {
    let offset = u64_at(bytes, 40);
    if offset < FILE_HEADER {
        return Err(Error::Corrupt);
    }
    Ok(BlockRef {
        store,
        digest: bytes[8..40].try_into().unwrap(),
        offset,
        spec: TileSpec::decode(&bytes[..8])?,
    })
}

fn digest_parts<'a>(
    kind: &[u8; 4],
    parts: impl IntoIterator<Item = &'a [u8]>,
    check: Check<'_>,
) -> Result<Digest> {
    let mut context = Context::new(&SHA256);
    context.update(kind);
    for bytes in parts {
        for chunk in bytes.chunks(CHUNK) {
            check()?;
            context.update(chunk);
        }
    }
    Ok(context.finish().as_ref().try_into().unwrap())
}

fn append_bytes(file: &File, offset: &mut u64, bytes: &[u8], check: Check<'_>) -> Result<()> {
    for chunk in bytes.chunks(CHUNK) {
        check()?;
        write_all_at(file, *offset, chunk)?;
        *offset = offset
            .checked_add(chunk.len() as u64)
            .ok_or(Error::Budget)?;
    }
    Ok(())
}
fn read_checked(file: &File, offset: u64, bytes: &mut [u8], check: Check<'_>) -> Result<()> {
    let mut at = offset;
    for chunk in bytes.chunks_mut(CHUNK) {
        check()?;
        read_exact_at(file, at, chunk)?;
        at = at.checked_add(chunk.len() as u64).ok_or(Error::Corrupt)?;
    }
    Ok(())
}

fn read_exact_at(file: &File, mut offset: u64, mut bytes: &mut [u8]) -> Result<()> {
    while !bytes.is_empty() {
        #[cfg(unix)]
        let n = {
            use std::os::unix::fs::FileExt;
            file.read_at(bytes, offset)?
        };
        #[cfg(windows)]
        let n = {
            use std::os::windows::fs::FileExt;
            file.seek_read(bytes, offset)?
        };
        if n == 0 {
            return Err(Error::Corrupt);
        }
        offset = offset.checked_add(n as u64).ok_or(Error::Corrupt)?;
        bytes = &mut bytes[n..];
    }
    Ok(())
}
fn write_all_at(file: &File, mut offset: u64, mut bytes: &[u8]) -> Result<()> {
    while !bytes.is_empty() {
        #[cfg(unix)]
        let n = {
            use std::os::unix::fs::FileExt;
            file.write_at(bytes, offset)?
        };
        #[cfg(windows)]
        let n = {
            use std::os::windows::fs::FileExt;
            file.seek_write(bytes, offset)?
        };
        if n == 0 {
            return Err(Error::Io(std::io::ErrorKind::WriteZero.into()));
        }
        offset = offset.checked_add(n as u64).ok_or(Error::Budget)?;
        bytes = &bytes[n..];
    }
    Ok(())
}
fn u16_at(bytes: &[u8], offset: usize) -> u16 {
    u16::from_be_bytes(bytes[offset..offset + 2].try_into().unwrap())
}
fn u32_at(bytes: &[u8], offset: usize) -> u32 {
    u32::from_be_bytes(bytes[offset..offset + 4].try_into().unwrap())
}
fn u64_at(bytes: &[u8], offset: usize) -> u64 {
    u64::from_be_bytes(bytes[offset..offset + 8].try_into().unwrap())
}

fn source_options() -> OpenOptions {
    let mut options = OpenOptions::new();
    options.read(true).write(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.share_mode(5).custom_flags(0x00200000);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    options
}
fn alias(metadata: &fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if metadata.file_attributes() & 0x400 != 0 {
            return true;
        }
    }
    metadata.file_type().is_symlink()
}
fn regular(path: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.is_file() || alias(&metadata) {
        return Err(Error::Changed);
    }
    Ok(())
}
fn validate_ancestors(path: &Path) -> Result<()> {
    for ancestor in path.ancestors() {
        let metadata = fs::symlink_metadata(ancestor)?;
        if !metadata.is_dir() || alias(&metadata) {
            return Err(Error::Changed);
        }
    }
    Ok(())
}
fn open_directory(path: &Path) -> Result<File> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.is_dir() || alias(&metadata) {
        return Err(Error::Changed);
    }
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options
            .access_mode(0)
            .share_mode(7)
            .custom_flags(0x02000000 | 0x00200000);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW);
    }
    Ok(options.open(path)?)
}

#[cfg(unix)]
fn links(file: &File) -> Result<u64> {
    use std::os::unix::fs::MetadataExt;
    Ok(file.metadata()?.nlink())
}
#[cfg(windows)]
fn links(file: &File) -> Result<u64> {
    use std::os::windows::io::AsRawHandle;
    #[repr(C)]
    #[derive(Default)]
    struct Info {
        attributes: u32,
        created: [u32; 2],
        accessed: [u32; 2],
        written: [u32; 2],
        volume: u32,
        size_high: u32,
        size_low: u32,
        links: u32,
        index_high: u32,
        index_low: u32,
    }
    #[link(name = "kernel32")]
    extern "system" {
        fn GetFileInformationByHandle(handle: *mut std::ffi::c_void, info: *mut Info) -> i32;
    }
    let mut info = Info::default();
    // SAFETY: The live File supplies a valid handle; Info has the Win32
    // BY_HANDLE_FILE_INFORMATION layout and remains writable for the whole call.
    if unsafe { GetFileInformationByHandle(file.as_raw_handle(), &mut info) } == 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    Ok(info.links as u64)
}

#[cfg(test)]
#[path = "tests.rs"]
mod tests;
