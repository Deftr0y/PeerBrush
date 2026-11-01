//! Native backing recovery is separate from PSD/Source36 standard-data proof.
//! These internal operations do not enable live native-document adoption.
use crate::{
    engine::scene::{Document36, PendingScene, RasterRoster, SceneSnapshot, StagedScene},
    large_document::{BlockRef, CommitRef, Error, ScratchBytes, Snapshot},
    psd::{
        canonical36::{Canonical, Limits as JsonLimits, Stats},
        memory36::{Admission, MetadataCharge, WorkingSet},
    },
    raster::{surface::TilePatch, Surface},
    workspace::source_runtime::{NativeMarker, SourceSession},
};
use ring::digest::{Context, SHA256};
use std::{
    io::{self, Read, Write},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
};

#[path = "backed/checkpoint.rs"]
mod checkpoint;
pub(crate) use checkpoint::{Parts as CheckpointParts, Proof as CheckpointProof};

type Result<T> = std::result::Result<T, String>;
const MAGIC: &[u8; 16] = b"PBRECOVERY3\0\0\0\0\0";
const DOMAIN: &[u8] = b"PeerBrushNativeBackingRecovery3\0";
const HEADER: u64 = 112;
const MAX_MARKER: u64 = 128 * 1024 * 1024;
const MAX_REFS: usize = 1_048_576;
const MAX_STORES: usize = 4096;

#[derive(Clone, Copy)]
pub(crate) struct Limits {
    pub metadata: JsonLimits,
    pub refs: usize,
    pub stores: usize,
}
impl Limits {
    fn validate(self) -> Result<()> {
        self.metadata.validate()?;
        if self.refs == 0 || self.refs > MAX_REFS || self.stores == 0 || self.stores > MAX_STORES {
            return Err("Native recovery count admission differs".into());
        }
        Ok(())
    }
}
struct Table {
    selected: Snapshot,
    blocks: Vec<BlockRef>,
    lookup: Vec<([u8; 32], u32)>,
    _owner: Arc<MetadataCharge>,
}
#[derive(Clone, Copy)]
struct Tile {
    x: u32,
    y: u32,
    store: u32,
    ordinal: u32,
}
struct Raster {
    id: u32,
    width: u32,
    height: u32,
    depth: u16,
    tiles: Vec<Tile>,
}

/// All captured metadata, physical references and exact selected prefixes remain
/// strongly owned until the last source check and durable marker publication.
pub(crate) struct Capture {
    project: [u8; 16],
    document: [u8; 16],
    revision: u64,
    timestamp: u64,
    metadata: Vec<u8>,
    tables: Vec<Table>,
    rasters: Vec<Raster>,
    failed: AtomicBool,
    _owners: [Arc<MetadataCharge>; 2],
}

fn io_guard<T>(
    session: &SourceSession,
    check: &dyn Fn() -> Result<()>,
    run: impl FnOnce(
        &mut dyn FnMut() -> crate::large_document::Result<()>,
    ) -> crate::large_document::Result<T>,
) -> Result<T> {
    let mut reason = None;
    let result = run(&mut || {
        if let Err(error) = check().and_then(|_| session.check()) {
            reason = Some(error);
            return Err(Error::Cancelled);
        }
        Ok(())
    });
    result.map_err(|e| reason.unwrap_or_else(|| e.to_string()))
}
fn table(
    snapshot: Snapshot,
    session: &SourceSession,
    maximum: usize,
    check: &dyn Fn() -> Result<()>,
) -> Result<Table> {
    let mut count = 0usize;
    io_guard(session, check, |guard| {
        snapshot.visit_blocks(guard, |_| {
            count = count
                .checked_add(1)
                .filter(|n| *n <= maximum)
                .ok_or(Error::Budget)?;
            Ok(())
        })
    })?;
    let bytes = count
        .checked_mul(std::mem::size_of::<BlockRef>() + std::mem::size_of::<([u8; 32], u32)>())
        .and_then(|n| n.checked_mul(2))
        .and_then(|n| n.checked_add(256))
        .ok_or("Native recovery table footprint overflow")?;
    let owner = MetadataCharge::reserve(&session.resident(), bytes, check)?;
    let mut blocks = Vec::new();
    let mut lookup = Vec::new();
    blocks
        .try_reserve_exact(count)
        .map_err(|_| "Native recovery table allocation refused")?;
    lookup
        .try_reserve_exact(count)
        .map_err(|_| "Native recovery lookup allocation refused")?;
    if blocks.capacity() > count.saturating_mul(2) || lookup.capacity() > count.saturating_mul(2) {
        return Err("Native recovery table exceeded its charged capacity".into());
    }
    io_guard(session, check, |guard| {
        snapshot.visit_blocks(guard, |reference| {
            if blocks.len() >= count {
                return Err(Error::Changed);
            }
            lookup.push((reference.fingerprint(), blocks.len() as u32));
            blocks.push(reference);
            Ok(())
        })
    })?;
    if blocks.len() != count {
        return Err("Native recovery selected table changed".into());
    }
    lookup.sort_unstable_by_key(|(key, _)| *key);
    if lookup.windows(2).any(|w| w[0].0 == w[1].0) {
        return Err("Native recovery selected table has duplicate references".into());
    }
    Ok(Table {
        selected: snapshot,
        blocks,
        lookup,
        _owner: owner,
    })
}

struct Count {
    bytes: usize,
    maximum: usize,
}
impl Write for Count {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.bytes = self
            .bytes
            .checked_add(bytes.len())
            .filter(|n| *n <= self.maximum)
            .ok_or_else(|| io::Error::other("Native recovery metadata byte admission exceeded"))?;
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Capture {
    pub(crate) fn from_staged(
        staged: &StagedScene<SourceSession>,
        session: &SourceSession,
        limits: Limits,
        check: &dyn Fn() -> Result<()>,
    ) -> Result<Self> {
        if let Some(held) = &staged.session {
            held.check()?;
        }
        Self::new(&staged.scene, &staged.seals, session, limits, check)
    }
    pub(crate) fn new(
        scene: &SceneSnapshot,
        seals: &[Snapshot],
        session: &SourceSession,
        limits: Limits,
        check: &dyn Fn() -> Result<()>,
    ) -> Result<Self> {
        limits.validate()?;
        check()?;
        session.check()?;
        if !scene.budget().same_pool(&session.resident())
            || !scene.disk_budget().same_pool(&session.disk())
            || seals.len() > limits.stores
        {
            return Err("Native recovery source admission differs from captured scene".into());
        }
        let project = *uuid::Uuid::parse_str(&scene.identity().project_id)
            .map_err(|_| "Invalid recovery project identity")?
            .as_bytes();
        let document = *uuid::Uuid::parse_str(&scene.identity().document_id)
            .map_err(|_| "Invalid recovery document identity")?
            .as_bytes();
        let mut count = Count {
            bytes: 0,
            maximum: limits.metadata.bytes,
        };
        serde_json::to_writer(&mut count, &scene.metadata()).map_err(|e| e.to_string())?;
        let raw_charge = MetadataCharge::reserve(
            scene.budget(),
            count
                .bytes
                .checked_mul(2)
                .ok_or("Recovery metadata footprint overflow")?,
            check,
        )?;
        let mut raw = Vec::new();
        raw.try_reserve_exact(count.bytes)
            .map_err(|_| "Recovery metadata allocation refused")?;
        if raw.capacity() > raw_charge.bytes() {
            return Err("Recovery metadata exceeds admitted capacity".into());
        }
        serde_json::to_writer(&mut raw, &scene.metadata()).map_err(|e| e.to_string())?;
        if raw.len() != count.bytes {
            return Err("Immutable recovery metadata serialization changed".into());
        }
        let stats = Stats::scan(&raw, limits.metadata, check)?;
        let canonical_bytes = stats
            .tree_upper_bytes()?
            .checked_mul(2)
            .and_then(|n| count.bytes.checked_mul(4).and_then(|b| n.checked_add(b)))
            .ok_or("Recovery canonical metadata footprint overflow")?;
        let canonical_owner = MetadataCharge::reserve(scene.budget(), canonical_bytes, check)?;
        let canonical = Canonical::from_serializable(
            &scene.metadata(),
            JsonLimits {
                bytes: count.bytes,
                ..limits.metadata
            },
            check,
        )?;
        let (metadata, value) = canonical.into_parts();
        let typed: Document36 = serde_json::from_value(value)
            .map_err(|e| format!("Captured recovery metadata is not adoptable Source36: {e}"))?;
        if typed.id != scene.identity().document_id || typed.revision != scene.identity().revision {
            return Err("Captured recovery metadata identity differs from its scene".into());
        }
        let count = typed.visit_rasters(|reference| {
            scene.rasters().get(reference)?;
            Ok(())
        })?;
        if count != scene.rasters().len() {
            return Err("Captured recovery metadata omits a raster source".into());
        }
        drop(typed);
        drop(raw);
        drop(raw_charge);
        let mut refs = 0usize;
        for (_, raster) in scene.rasters().iter() {
            check()?;
            refs = refs
                .checked_add(raster.physical_tiles().count())
                .filter(|n| *n <= limits.refs)
                .ok_or("Native recovery reference limit exceeded")?;
        }
        let roster_len = scene.rasters().len();
        let index_bytes = refs
            .checked_mul(2 * std::mem::size_of::<Tile>())
            .and_then(|n| {
                roster_len
                    .checked_mul(2 * std::mem::size_of::<Raster>())
                    .and_then(|r| n.checked_add(r))
            })
            .and_then(|n| {
                seals
                    .len()
                    .checked_mul(2 * std::mem::size_of::<Table>())
                    .and_then(|r| n.checked_add(r))
            })
            .and_then(|n| n.checked_add(256))
            .ok_or("Native recovery binding footprint overflow")?;
        let index_owner = MetadataCharge::reserve(scene.budget(), index_bytes, check)?;
        let mut tables = Vec::new();
        tables
            .try_reserve_exact(seals.len())
            .map_err(|_| "Recovery source roster allocation refused")?;
        if tables.capacity() > seals.len().saturating_mul(2) {
            return Err("Recovery source roster capacity differs".into());
        }
        let mut total_refs = 0;
        for seal in seals {
            check()?;
            let owner = seal.store();
            let selected = session.resolve_store(owner.identity(), seal.reference())?;
            if !selected.store().same_owner(&owner) {
                return Err("Recovery selected prefix belongs to another owner".into());
            }
            let selected = table(selected, session, limits.refs - total_refs, check)?;
            total_refs += selected.blocks.len();
            tables.push(selected);
        }
        tables.sort_unstable_by_key(|t| t.selected.store().identity());
        if tables
            .windows(2)
            .any(|w| w[0].selected.store().identity() == w[1].selected.store().identity())
        {
            return Err("Recovery source roster repeats an owner".into());
        }
        let mut rasters = Vec::new();
        rasters
            .try_reserve_exact(roster_len)
            .map_err(|_| "Recovery raster roster allocation refused")?;
        if rasters.capacity() > roster_len.saturating_mul(2) {
            return Err("Recovery raster capacity differs".into());
        }
        let mut referenced = Vec::new();
        referenced
            .try_reserve_exact(tables.len())
            .map_err(|_| "Recovery owner coverage allocation refused")?;
        referenced.resize(tables.len(), false);
        for (id, raster) in scene.rasters().iter() {
            check()?;
            let mut tiles = Vec::new();
            let count = raster.physical_tiles().count();
            tiles
                .try_reserve_exact(count)
                .map_err(|_| "Recovery tile bindings allocation refused")?;
            if tiles.capacity() > count.saturating_mul(2) {
                return Err("Recovery tile binding capacity differs".into());
            }
            for physical in raster.physical_tiles() {
                let store = tables
                    .binary_search_by_key(&physical.owner().identity(), |t| {
                        t.selected.store().identity()
                    })
                    .map_err(|_| "Recovery raster has no selected owner prefix")?;
                let lookup = &tables[store].lookup;
                let at = lookup
                    .binary_search_by_key(&physical.reference().fingerprint(), |(key, _)| *key)
                    .map_err(|_| "Recovery raster reference is absent from its selected table")?;
                let (x, y) = physical.coordinate();
                referenced[store] = true;
                tiles.push(Tile {
                    x,
                    y,
                    store: store as u32,
                    ordinal: lookup[at].1,
                });
            }
            rasters.push(Raster {
                id,
                width: raster.width(),
                height: raster.height(),
                depth: raster.depth(),
                tiles,
            });
        }
        if referenced.iter().any(|v| !v) {
            return Err("Recovery source roster has an unused owner".into());
        }
        check()?;
        session.check()?;
        Ok(Self {
            project,
            document,
            revision: scene.identity().revision,
            timestamp: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_err(|_| "Recovery clock is unavailable")?
                .as_millis()
                .try_into()
                .map_err(|_| "Recovery clock overflow")?,
            metadata,
            tables,
            rasters,
            failed: AtomicBool::new(false),
            _owners: [canonical_owner, index_owner],
        })
    }
    fn write_body(&self, snapshot: [u8; 16], output: &mut dyn Write) -> Result<()> {
        let refs = self
            .rasters
            .iter()
            .map(|r| r.tiles.len() as u64)
            .sum::<u64>();
        for bytes in [
            MAGIC.as_slice(),
            &3u32.to_be_bytes(),
            &1u32.to_be_bytes(),
            &snapshot,
            &self.project,
            &self.document,
            &self.revision.to_be_bytes(),
            &self.timestamp.to_be_bytes(),
            &(self.metadata.len() as u64).to_be_bytes(),
            &(self.tables.len() as u32).to_be_bytes(),
            &(self.rasters.len() as u32).to_be_bytes(),
            &refs.to_be_bytes(),
        ] {
            output.write_all(bytes).map_err(|e| e.to_string())?;
        }
        output
            .write_all(&self.metadata)
            .map_err(|e| e.to_string())?;
        for table in &self.tables {
            output
                .write_all(&table.selected.store().identity())
                .map_err(|e| e.to_string())?;
            output
                .write_all(&table.selected.reference().encode())
                .map_err(|e| e.to_string())?;
        }
        for raster in &self.rasters {
            for bytes in [
                &raster.id.to_be_bytes()[..],
                &raster.width.to_be_bytes(),
                &raster.height.to_be_bytes(),
                &raster.depth.to_be_bytes(),
                &0u16.to_be_bytes(),
                &(raster.tiles.len() as u32).to_be_bytes(),
            ] {
                output.write_all(bytes).map_err(|e| e.to_string())?;
            }
            for tile in &raster.tiles {
                for value in [tile.x, tile.y, tile.store, tile.ordinal] {
                    output
                        .write_all(&value.to_be_bytes())
                        .map_err(|e| e.to_string())?;
                }
            }
        }
        Ok(())
    }
    /// This is intentionally not called by the live resident-Document worker.
    /// Native engine adoption and complete recovery ownership dispatch are gates.
    pub(crate) fn publish(
        &self,
        snapshot: [u8; 16],
        session: &SourceSession,
        check: &dyn Fn() -> Result<()>,
    ) -> Result<()> {
        if snapshot == [0; 16] {
            return Err("Recovery marker identity is empty".into());
        }
        check()?;
        session.check()?;
        if self.failed.load(Ordering::Acquire)
            || self
                ._owners
                .iter()
                .any(|owner| !owner.uses_budget(&session.resident()))
        {
            return Err(
                "Recovery publication was refused or belongs to another source runtime".into(),
            );
        }
        let mut counted = HashWriter {
            bytes: 0,
            hash: Context::new(&SHA256),
        };
        counted.hash.update(DOMAIN);
        self.write_body(snapshot, &mut counted)?;
        let payload_sha: [u8; 32] = counted.hash.finish().as_ref().try_into().unwrap();
        let expected = HEADER
            + self.metadata.len() as u64
            + self.tables.len() as u64 * 112
            + self.rasters.len() as u64 * 20
            + self
                .rasters
                .iter()
                .map(|r| r.tiles.len() as u64 * 16)
                .sum::<u64>();
        if counted.bytes != expected || expected.checked_add(32).is_none_or(|n| n > MAX_MARKER) {
            return Err("Recovery marker framing exceeds its declared bounds".into());
        }
        let mut full = HashWriter {
            bytes: 0,
            hash: Context::new(&SHA256),
        };
        self.write_body(snapshot, &mut full)?;
        full.write_all(&payload_sha).map_err(|e| e.to_string())?;
        let sha256 = full.hash.finish().as_ref().try_into().unwrap();
        let proof = CheckpointProof::from_validated(
            snapshot,
            full.bytes,
            sha256,
            self.tables.iter().map(|table| table.selected.clone()),
            session,
            check,
        )
        .inspect_err(|_| {
            self.failed.store(true, Ordering::Release);
        })?;
        let mut scratch = ScratchBytes::new(&session.resident(), 64 * 1024, 64 * 1024)
            .map_err(|e| e.to_string())?;
        let result = session.publish_recovery_marker(
            snapshot,
            full.bytes,
            sha256,
            &mut |output| {
                let mut buffered = Buffered {
                    output,
                    scratch: &mut scratch,
                    used: 0,
                };
                self.write_body(snapshot, &mut buffered)?;
                buffered
                    .write_all(&payload_sha)
                    .map_err(|e| e.to_string())?;
                buffered.flush().map_err(|e| e.to_string())
            },
            &|| {
                check()?;
                for table in &self.tables {
                    io_guard(session, check, |guard| table.selected.check_owner(guard))?;
                }
                Ok(())
            },
        );
        let result = result.and_then(|_| {
            let mut marker = session.open_recovery_marker(snapshot)?;
            session.register_checkpoint(proof, &mut marker, check)
        });
        if result.is_err() {
            self.failed.store(true, Ordering::Release);
        }
        result
    }
}
struct Buffered<'a> {
    output: &'a mut dyn Write,
    scratch: &'a mut ScratchBytes,
    used: usize,
}
impl Write for Buffered<'_> {
    fn write(&mut self, mut bytes: &[u8]) -> io::Result<usize> {
        let length = bytes.len();
        while !bytes.is_empty() {
            let count = bytes.len().min(self.scratch.len() - self.used);
            self.scratch[self.used..self.used + count].copy_from_slice(&bytes[..count]);
            self.used += count;
            bytes = &bytes[count..];
            if self.used == self.scratch.len() {
                self.flush()?;
            }
        }
        Ok(length)
    }
    fn flush(&mut self) -> io::Result<()> {
        if self.used != 0 {
            self.output.write_all(&self.scratch[..self.used])?;
            self.used = 0;
        }
        self.output.flush()
    }
}
struct HashWriter {
    bytes: u64,
    hash: Context,
}

/// Provisional until the actual engine's guarded adoption retains every source
/// and selected prefix. A valid marker alone grants no editable registry entry.
pub(crate) struct DecodedNativeRecovery<'a> {
    pub pending: PendingScene,
    pub seals: Vec<Snapshot>,
    pub marker: NativeMarker<'a>,
    pub project: [u8; 16],
    pub timestamp: u64,
    _owner: Arc<MetadataCharge>,
}
struct Input<'a> {
    marker: NativeMarker<'a>,
    scratch: ScratchBytes,
    at: usize,
    filled: usize,
    consumed: u64,
    hash: Context,
    whole: Context,
}
impl<'a> Input<'a> {
    fn new(marker: NativeMarker<'a>, session: &SourceSession) -> Result<Self> {
        let scratch = ScratchBytes::new(&session.resident(), 64 * 1024, 64 * 1024)
            .map_err(|e| e.to_string())?;
        let mut hash = Context::new(&SHA256);
        hash.update(DOMAIN);
        Ok(Self {
            marker,
            scratch,
            at: 0,
            filled: 0,
            consumed: 0,
            hash,
            whole: Context::new(&SHA256),
        })
    }
    fn read_into(&mut self, mut output: &mut [u8], hashed: bool) -> Result<()> {
        while !output.is_empty() {
            if self.at == self.filled {
                self.filled = self
                    .marker
                    .read(&mut self.scratch)
                    .map_err(|_| "Native recovery marker read failed")?;
                self.at = 0;
                if self.filled == 0 {
                    return Err("Native recovery marker is truncated".into());
                }
            }
            let count = output.len().min(self.filled - self.at);
            output[..count].copy_from_slice(&self.scratch[self.at..self.at + count]);
            self.whole.update(&output[..count]);
            if hashed {
                self.hash.update(&output[..count]);
            }
            self.at += count;
            self.consumed = self
                .consumed
                .checked_add(count as u64)
                .ok_or("Recovery marker length overflow")?;
            output = &mut output[count..];
        }
        Ok(())
    }
    fn fixed<const N: usize>(&mut self) -> Result<[u8; N]> {
        let mut bytes = [0; N];
        self.read_into(&mut bytes, true)?;
        Ok(bytes)
    }
    fn finish(mut self, expected: u64) -> Result<(NativeMarker<'a>, [u8; 32])> {
        let mut footer = [0; 32];
        self.read_into(&mut footer, false)?;
        if self.consumed != expected
            || self.at != self.filled
            || self.hash.finish().as_ref() != footer
        {
            return Err("Native recovery full framing SHA or length differs".into());
        }
        let mut extra = [0; 1];
        if self
            .marker
            .read(&mut extra)
            .map_err(|_| "Recovery marker final read failed")?
            != 0
        {
            return Err("Native recovery marker has a trailing record".into());
        }
        self.marker.check()?;
        Ok((
            self.marker,
            self.whole.finish().as_ref().try_into().unwrap(),
        ))
    }
}

pub(crate) fn load<'a>(
    session: &'a SourceSession,
    snapshot: [u8; 16],
    limits: Limits,
    check: &dyn Fn() -> Result<()>,
) -> Result<DecodedNativeRecovery<'a>> {
    limits.validate()?;
    check()?;
    let marker = session.open_recovery_marker(snapshot)?;
    let length = marker.len();
    let mut input = Input::new(marker, session)?;
    if &input.fixed::<16>()? != MAGIC
        || u32::from_be_bytes(input.fixed()?) != 3
        || u32::from_be_bytes(input.fixed()?) != 1
        || input.fixed::<16>()? != snapshot
    {
        return Err("Unknown native recovery marker framing retained".into());
    }
    let project = input.fixed::<16>()?;
    let document = input.fixed::<16>()?;
    let revision = u64::from_be_bytes(input.fixed()?);
    let timestamp = u64::from_be_bytes(input.fixed()?);
    let metadata_len = usize::try_from(u64::from_be_bytes(input.fixed()?))
        .map_err(|_| "Recovery metadata length overflow")?;
    let store_count = u32::from_be_bytes(input.fixed()?) as usize;
    let raster_count = u32::from_be_bytes(input.fixed()?) as usize;
    let refs = usize::try_from(u64::from_be_bytes(input.fixed()?))
        .map_err(|_| "Recovery reference count overflow")?;
    if project == [0; 16]
        || document == [0; 16]
        || metadata_len == 0
        || metadata_len > limits.metadata.bytes
        || store_count > limits.stores
        || raster_count > crate::engine::scene::MAX_RASTER_LOCATIONS
        || refs > limits.refs
    {
        return Err("Native recovery header admission differs".into());
    }
    let expected = HEADER
        .checked_add(metadata_len as u64)
        .and_then(|n| n.checked_add(store_count as u64 * 112))
        .and_then(|n| n.checked_add(raster_count as u64 * 20))
        .and_then(|n| n.checked_add(refs as u64 * 16))
        .and_then(|n| n.checked_add(32))
        .filter(|n| *n <= MAX_MARKER && *n == length)
        .ok_or("Native recovery total length differs")?;
    let raw_owner = MetadataCharge::reserve(
        &session.resident(),
        metadata_len
            .checked_mul(2)
            .ok_or("Recovery metadata footprint overflow")?,
        check,
    )?;
    let mut raw = Vec::new();
    raw.try_reserve_exact(metadata_len)
        .map_err(|_| "Recovery metadata allocation refused")?;
    if raw.capacity() > raw_owner.bytes() {
        return Err("Recovery metadata allocation exceeds admission".into());
    }
    raw.resize(metadata_len, 0);
    input.read_into(&mut raw, true)?;
    let index_bytes = refs
        .checked_mul(2 * std::mem::size_of::<Tile>())
        .and_then(|n| {
            raster_count
                .checked_mul(2 * (std::mem::size_of::<Raster>() + std::mem::size_of::<Surface>()))
                .and_then(|r| n.checked_add(r))
        })
        .and_then(|n| {
            store_count
                .checked_mul(2 * (std::mem::size_of::<Table>() + 112))
                .and_then(|s| n.checked_add(s))
        })
        .and_then(|n| n.checked_add(1024))
        .ok_or("Recovery index admission overflow")?;
    let owner = MetadataCharge::reserve(&session.resident(), index_bytes, check)?;
    let mut selected = Vec::new();
    selected
        .try_reserve_exact(store_count)
        .map_err(|_| "Recovery source table allocation refused")?;
    if selected.capacity() > store_count.saturating_mul(2) {
        return Err("Recovery source capacity differs".into());
    }
    let mut previous_store = None;
    for _ in 0..store_count {
        check()?;
        let identity = input.fixed::<16>()?;
        let bytes = input.fixed::<96>()?;
        if identity == [0; 16]
            || bytes[8..24] != identity
            || previous_store.is_some_and(|p| p >= identity)
        {
            return Err("Recovery source identities are duplicate or unordered".into());
        }
        previous_store = Some(identity);
        selected.push((
            identity,
            CommitRef::decode(&bytes).map_err(|e| e.to_string())?,
        ));
    }
    let mut rasters = Vec::new();
    rasters
        .try_reserve_exact(raster_count)
        .map_err(|_| "Recovery raster bindings allocation refused")?;
    if rasters.capacity() > raster_count.saturating_mul(2) {
        return Err("Recovery raster capacity differs".into());
    }
    let mut remaining = refs;
    for index in 0..raster_count {
        check()?;
        let id = u32::from_be_bytes(input.fixed()?);
        let width = u32::from_be_bytes(input.fixed()?);
        let height = u32::from_be_bytes(input.fixed()?);
        let depth = u16::from_be_bytes(input.fixed()?);
        let flags = u16::from_be_bytes(input.fixed()?);
        let count = u32::from_be_bytes(input.fixed()?) as usize;
        if id as usize != index + 1
            || width == 0
            || height == 0
            || width > 16000
            || height > 16000
            || !matches!(depth, 8 | 16)
            || flags != 0
            || count > remaining
            || count > width.div_ceil(256) as usize * height.div_ceil(256) as usize
        {
            return Err("Recovery raster shape or complete ordered roster differs".into());
        }
        remaining -= count;
        let mut tiles = Vec::new();
        tiles
            .try_reserve_exact(count)
            .map_err(|_| "Recovery native references allocation refused")?;
        if tiles.capacity() > count.saturating_mul(2) {
            return Err("Recovery tile capacity differs".into());
        }
        let mut previous = None;
        for _ in 0..count {
            let x = u32::from_be_bytes(input.fixed()?);
            let y = u32::from_be_bytes(input.fixed()?);
            let store = u32::from_be_bytes(input.fixed()?);
            let ordinal = u32::from_be_bytes(input.fixed()?);
            if x >= width.div_ceil(256)
                || y >= height.div_ceil(256)
                || store as usize >= store_count
                || previous.is_some_and(|p| p >= (x, y))
            {
                return Err(
                    "Recovery raster coordinates are missing, duplicate or unordered".into(),
                );
            }
            previous = Some((x, y));
            tiles.push(Tile {
                x,
                y,
                store,
                ordinal,
            });
        }
        rasters.push(Raster {
            id,
            width,
            height,
            depth,
            tiles,
        });
    }
    if remaining != 0 {
        return Err("Recovery reference total does not match its complete roster".into());
    }
    let (mut marker, marker_sha256) = input.finish(expected)?;
    check()?;
    let stats = Stats::scan(&raw, limits.metadata, check)?;
    let capacity = metadata_len
        .checked_next_power_of_two()
        .unwrap_or(crate::psd::canonical36::MAX_BYTES)
        .min(crate::psd::canonical36::MAX_BYTES);
    let working = WorkingSet {
        raw_bytes: raw.capacity(),
        canonical_core_bytes: capacity,
        canonical_manifest_bytes: capacity,
        typed_metadata_bytes: stats.tree_upper_bytes()?,
        tile_scratch_bytes: 0,
        index_scratch_bytes: 0,
    };
    let admitted = Admission::reserve(
        stats,
        limits.metadata,
        working,
        &session.resident(),
        &raw_owner,
        check,
    )?
    .parse_owned(raw, check)?;
    drop(raw_owner);
    let metadata = admitted.map_unique(|canonical, _| {
        let (bytes, value) = canonical.into_parts();
        drop(bytes);
        let metadata: Document36 = serde_json::from_value(value)
            .map_err(|e| format!("Invalid typed recovery metadata: {e}"))?;
        if metadata.revision != revision
            || uuid::Uuid::parse_str(&metadata.id)
                .ok()
                .map(|v| *v.as_bytes())
                != Some(document)
        {
            return Err("Recovery typed document identity differs".into());
        }
        Ok(metadata)
    })?;
    let mut tables = Vec::new();
    tables
        .try_reserve_exact(store_count)
        .map_err(|_| "Recovery resolved sources allocation refused")?;
    if tables.capacity() > store_count.saturating_mul(2) {
        return Err("Recovery resolved source capacity differs".into());
    }
    let mut total_refs = 0;
    for (identity, reference) in selected {
        check()?;
        let snapshot = session.resolve_store(identity, reference)?;
        let table = table(snapshot, session, limits.refs - total_refs, check)?;
        total_refs += table.blocks.len();
        tables.push(table);
    }
    let mut referenced = Vec::new();
    referenced
        .try_reserve_exact(store_count)
        .map_err(|_| "Recovery source coverage allocation refused")?;
    referenced.resize(store_count, false);
    let mut surfaces = Vec::new();
    surfaces
        .try_reserve_exact(raster_count)
        .map_err(|_| "Recovery source roster allocation refused")?;
    if surfaces.capacity() > raster_count.saturating_mul(2) {
        return Err("Recovery source roster capacity differs".into());
    }
    for raster in rasters {
        check()?;
        let mut patches = Vec::new();
        let patch_bytes = raster
            .tiles
            .len()
            .checked_mul(std::mem::size_of::<TilePatch>())
            .and_then(|n| n.checked_mul(2))
            .and_then(|n| n.checked_add(128))
            .ok_or("Recovery source patches overflow")?;
        let patch_owner = MetadataCharge::reserve(&session.resident(), patch_bytes, check)?;
        patches
            .try_reserve_exact(raster.tiles.len())
            .map_err(|_| "Recovery source patch allocation refused")?;
        if patches.capacity() > raster.tiles.len().saturating_mul(2) {
            return Err("Recovery patch capacity differs".into());
        }
        for tile in raster.tiles {
            let table = &tables[tile.store as usize];
            let reference = *table
                .blocks
                .get(tile.ordinal as usize)
                .ok_or("Recovery reference ordinal is absent from its selected prefix")?;
            if reference.spec().width() != 256
                || reference.spec().height() != 256
                || reference.spec().depth() != raster.depth
            {
                return Err("Recovery native block shape differs from its raster".into());
            }
            let store = table.selected.store();
            // No referenced failure becomes an absent transparent tile.
            drop(io_guard(session, check, |guard| {
                store.pin(reference, guard)
            })?);
            referenced[tile.store as usize] = true;
            patches.push(TilePatch::stored((tile.x, tile.y), store, reference));
        }
        let zero =
            Surface::zero(raster.width, raster.height, raster.depth).map_err(|e| e.to_string())?;
        let source = zero
            .with_tile_patches(&patches, &session.resident(), &session.disk())
            .map_err(|e| e.to_string())?;
        drop(patches);
        drop(patch_owner);
        surfaces.push(source);
    }
    if referenced.iter().any(|v| !v) {
        return Err("Recovery contains an unreferenced source owner".into());
    }
    let mut seals = Vec::new();
    seals
        .try_reserve_exact(tables.len())
        .map_err(|_| "Recovery selected-prefix roster allocation refused")?;
    if seals.capacity() > store_count.saturating_mul(2) {
        return Err("Recovery selected-prefix capacity differs".into());
    }
    for table in tables {
        io_guard(session, check, |guard| table.selected.check_owner(guard))?;
        seals.push(table.selected);
    }
    let pending = PendingScene::new(
        metadata,
        RasterRoster::new(surfaces)?,
        session.resident(),
        session.disk(),
    )?;
    marker.check()?;
    check()?;
    let proof = CheckpointProof::from_validated(
        snapshot,
        length,
        marker_sha256,
        seals.iter().cloned(),
        session,
        check,
    )?;
    session.register_checkpoint(proof, &mut marker, check)?;
    Ok(DecodedNativeRecovery {
        pending,
        seals,
        marker,
        project,
        timestamp,
        _owner: owner,
    })
}
impl Write for HashWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.bytes = self
            .bytes
            .checked_add(bytes.len() as u64)
            .filter(|n| *n <= MAX_MARKER)
            .ok_or_else(|| io::Error::other("Recovery marker byte limit exceeded"))?;
        self.hash.update(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        engine::{
            scene::{LegacyTileCache, StageRuntime, StageSession},
            Document, Engine,
        },
        large_document::{
            root::{Directory, PersistentRootContext},
            DiskBudget, ResidentBudget, Store,
        },
        workspace::source_runtime::{Limits as RuntimeLimits, SourceRuntime},
    };
    use std::{fs, path::PathBuf, sync::Mutex};
    struct Fixture {
        base: PathBuf,
        held: Option<Directory>,
        runtime: Option<Arc<SourceRuntime>>,
        connection: Option<crate::server::Connection>,
    }
    impl Fixture {
        fn new() -> Self {
            let base = fs::canonicalize(std::env::temp_dir())
                .unwrap()
                .join(format!(
                    "peerbrush-native-recovery-{}",
                    uuid::Uuid::new_v4()
                ));
            fs::create_dir(&base).unwrap();
            let held = Some(Directory::open(&base).unwrap());
            let shared = Arc::new(Mutex::new(Engine::new()));
            let connection = crate::server::start(shared, base.join("state")).unwrap();
            let context = PersistentRootContext::from_explicit_state(&connection).unwrap();
            let runtime = SourceRuntime::new(
                context,
                RuntimeLimits {
                    resident_bytes: 128 * 1024 * 1024,
                    disk_bytes: 16 * 1024 * 1024,
                    max_files: 32,
                    store: crate::large_document::Limits {
                        disk_bytes: 8 * 1024 * 1024,
                        max_blocks: 128,
                        max_resident_blocks: 16,
                        manifest_bytes: 1024 * 1024,
                    },
                },
            )
            .unwrap();
            Self {
                base,
                held,
                runtime: Some(runtime),
                connection: Some(connection),
            }
        }
        fn runtime(&self) -> &Arc<SourceRuntime> {
            self.runtime.as_ref().unwrap()
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            drop(self.runtime.take());
            drop(self.connection.take());
            let temp = fs::canonicalize(std::env::temp_dir()).unwrap();
            if self.base.starts_with(temp)
                && self.base.file_name().is_some_and(|n| {
                    n.to_string_lossy()
                        .starts_with("peerbrush-native-recovery-")
                })
                && self.held.as_ref().is_some_and(|held| held.check().is_ok())
                && fs::canonicalize(&self.base).is_ok_and(|p| p == self.base)
            {
                drop(self.held.take());
                fs::remove_dir_all(&self.base).unwrap();
            }
        }
    }
    struct Runtime(Arc<SourceRuntime>);
    struct Session(SourceSession);
    impl StageSession for Session {
        fn check(&self) -> Result<()> {
            self.0.check()
        }
        fn store_for_lineage(&self, lineage: [u8; 16]) -> Result<Store> {
            self.0.store_for_lineage(lineage)
        }
        fn seal_store(
            &self,
            store: &Store,
            refs: &[BlockRef],
            metadata: &[u8],
        ) -> Result<Snapshot> {
            self.0.seal_store(store, refs, metadata)
        }
    }
    impl StageRuntime for Runtime {
        type Session = Session;
        fn resident(&self) -> ResidentBudget {
            self.0.resident()
        }
        fn disk(&self) -> DiskBudget {
            self.0.disk()
        }
        fn begin(&self) -> Result<Session> {
            Ok(Session(self.0.begin()?))
        }
    }
    fn limits() -> Limits {
        Limits {
            metadata: JsonLimits {
                bytes: 65536,
                nodes: 8192,
                depth: 64,
            },
            refs: 1024,
            stores: 16,
        }
    }
    fn stage(f: &Fixture, depth: u16) -> StagedScene<Session> {
        let mut engine = Engine::new();
        engine.doc = Document::new_depth(512, 256, depth).unwrap();
        engine.doc.layers[0]
            .pixels
            .set16(2, 3, [12345, 23456, 34567, 45678]);
        engine.doc.layers[0]
            .pixels
            .set16(511, 255, [65535, 32768, 1, 65535]);
        let original = engine.doc.layers[0].pixels.clone();
        engine.doc.layers[0].pixels.retained = Some(Box::new(crate::retained::Original {
            pixels: Box::new(original),
            matrix: crate::retained::IDENTITY,
            projective: None,
            warps: vec![],
        }));
        engine.doc.layers[0].effects.push(crate::effects::Effect {
            id: crate::engine::id(),
            kind: "blur".into(),
            enabled: true,
            weight: 0.75,
            settings: Arc::new(serde_json::json!({"radius": 2.0})),
        });
        let mut mask = crate::raster::Raster::new_depth(512, 256, depth);
        mask.set16(3, 2, [32768; 4]);
        engine.doc.layers[0].mask = Some(crate::engine::Mask {
            enabled: true,
            linked: true,
            cache_key: crate::engine::id(),
            steps: vec![crate::engine::MaskStep {
                id: crate::engine::id(),
                kind: "paint".into(),
                enabled: true,
                weight: 0.75,
                value: 255.0,
                pixels: mask,
                settings: serde_json::json!({}),
            }],
        });
        let mut mask = crate::raster::Raster::new(512, 256);
        mask.set(4, 5, [191; 4]);
        let coverage = crate::selection::Coverage {
            mask,
            bounds: [0, 0, 512, 256],
            origin: [0, 0],
            contours: vec![vec![[0.0, 0.0], [512.0, 0.0], [512.0, 256.0]]],
        };
        engine.doc.selection = Some(coverage.bounds);
        engine.doc.selection_coverage = Some(coverage.clone());
        engine.doc.selection_previous = Some(coverage);
        let cache = LegacyTileCache::new(&f.runtime().resident(), 32).unwrap();
        engine
            .capture_scene(None)
            .unwrap()
            .stage(&Runtime(f.runtime().clone()), &cache, &|| Ok(()))
            .unwrap()
    }
    #[test]
    fn native8_and16_marker_roundtrips_actual_profile_bound_selected_sources() {
        for depth in [8, 16] {
            let f = Fixture::new();
            let staged = stage(&f, depth);
            let session = f.runtime().begin().unwrap();
            let capture =
                Capture::new(&staged.scene, &staged.seals, &session, limits(), &|| Ok(())).unwrap();
            let id = *uuid::Uuid::new_v4().as_bytes();
            capture.publish(id, &session, &|| Ok(())).unwrap();
            let decoded = load(&session, id, limits(), &|| Ok(())).unwrap();
            let original =
                Canonical::from_serializable(&staged.scene.metadata(), limits().metadata, &|| {
                    Ok(())
                })
                .unwrap();
            let restored = Canonical::from_serializable(
                decoded.pending.metadata(),
                limits().metadata,
                &|| Ok(()),
            )
            .unwrap();
            assert_eq!(original.bytes(), restored.bytes());
            assert_eq!(decoded.project, capture.project);
            assert_eq!(decoded.timestamp, capture.timestamp);
            assert_eq!(decoded.seals.len(), staged.seals.len());
            assert_eq!(decoded.marker.snapshot(), id);
            for ((id, source), (restored_id, restored)) in staged
                .scene
                .rasters()
                .iter()
                .zip(decoded.pending.rasters().iter())
            {
                assert_eq!(id, restored_id);
                let region = crate::raster::backed::Region::new([0, 0, 512, 256]).unwrap();
                let before = source.prepare_region(region, &mut || Ok(())).unwrap();
                let after = restored.prepare_region(region, &mut || Ok(())).unwrap();
                for y in 0..256 {
                    for x in 0..512 {
                        assert_eq!(before.pixel16(x, y).unwrap(), after.pixel16(x, y).unwrap());
                    }
                }
            }
        }
    }
    fn marker_bytes(session: &SourceSession, id: [u8; 16]) -> Vec<u8> {
        let mut marker = session.open_recovery_marker(id).unwrap();
        assert!(marker.len() < 65536);
        let mut bytes = Vec::new();
        marker.read_to_end(&mut bytes).unwrap();
        bytes
    }
    fn rewritten(
        session: &SourceSession,
        mut bytes: Vec<u8>,
        mutate: impl FnOnce(&mut [u8]),
    ) -> [u8; 16] {
        let id = *uuid::Uuid::new_v4().as_bytes();
        bytes[24..40].copy_from_slice(&id);
        let footer = bytes.len() - 32;
        mutate(&mut bytes[..footer]);
        let mut hash = Context::new(&SHA256);
        hash.update(DOMAIN);
        hash.update(&bytes[..footer]);
        bytes[footer..].copy_from_slice(hash.finish().as_ref());
        let sha = ring::digest::digest(&SHA256, &bytes)
            .as_ref()
            .try_into()
            .unwrap();
        session
            .publish_recovery_marker(
                id,
                bytes.len() as u64,
                sha,
                &mut |out| out.write_all(&bytes).map_err(|e| e.to_string()),
                &|| Ok(()),
            )
            .unwrap();
        id
    }
    #[test]
    fn valid_outer_sha_cannot_admit_missing_ordinals_duplicate_coords_or_count_bombs() {
        let f = Fixture::new();
        let staged = stage(&f, 16);
        let session = f.runtime().begin().unwrap();
        let capture =
            Capture::new(&staged.scene, &staged.seals, &session, limits(), &|| Ok(())).unwrap();
        let id = *uuid::Uuid::new_v4().as_bytes();
        capture.publish(id, &session, &|| Ok(())).unwrap();
        let original = marker_bytes(&session, id);
        let meta = u64::from_be_bytes(original[88..96].try_into().unwrap()) as usize;
        let stores = u32::from_be_bytes(original[96..100].try_into().unwrap()) as usize;
        let tile = HEADER as usize + meta + stores * 112 + 20;
        let ordinal = rewritten(&session, original.clone(), |bytes| {
            bytes[tile + 12..tile + 16].copy_from_slice(&u32::MAX.to_be_bytes())
        });
        assert!(load(&session, ordinal, limits(), &|| Ok(())).is_err());
        let coordinate = rewritten(&session, original.clone(), |bytes| {
            bytes[tile..tile + 4].copy_from_slice(&1u32.to_be_bytes())
        });
        assert!(load(&session, coordinate, limits(), &|| Ok(())).is_err());
        let count = rewritten(&session, original, |bytes| {
            bytes[104..112].copy_from_slice(&u64::MAX.to_be_bytes())
        });
        assert!(load(&session, count, limits(), &|| Ok(())).is_err());
    }
    #[test]
    fn stale_capture_and_foreign_runtime_cannot_publish_or_erase_selected_work() {
        let f = Fixture::new();
        let staged = stage(&f, 16);
        let session = f.runtime().begin().unwrap();
        let capture =
            Capture::new(&staged.scene, &staged.seals, &session, limits(), &|| Ok(())).unwrap();
        let foreign = Fixture::new();
        let other = foreign.runtime().begin().unwrap();
        assert!(capture
            .publish(*uuid::Uuid::new_v4().as_bytes(), &other, &|| Ok(()))
            .is_err());
        let id = *uuid::Uuid::new_v4().as_bytes();
        assert!(capture
            .publish(id, &session, &|| Err("Captured revision changed".into()))
            .is_err());
        assert!(session.open_recovery_marker(id).is_err());
        assert!(staged
            .seals
            .iter()
            .all(|s| s.check_owner(&mut || Ok(())).is_ok()));
    }
    #[test]
    fn failed_durable_capture_latches_and_keeps_its_pending_bytes_unselected() {
        let f = Fixture::new();
        let staged = stage(&f, 16);
        let session = f.runtime().begin().unwrap();
        let capture =
            Capture::new(&staged.scene, &staged.seals, &session, limits(), &|| Ok(())).unwrap();
        let id = *uuid::Uuid::new_v4().as_bytes();
        let calls = std::cell::Cell::new(0);
        assert!(capture
            .publish(id, &session, &|| {
                calls.set(calls.get() + 1);
                if calls.get() == 6 {
                    Err("Captured source changed during publication".into())
                } else {
                    Ok(())
                }
            })
            .is_err());
        assert!(f
            .base
            .join(format!(
                "state/native-sources-v1/pending-{}",
                uuid::Uuid::from_bytes(id)
            ))
            .is_file());
        assert!(session.open_recovery_marker(id).is_err());
        assert!(capture
            .publish(*uuid::Uuid::new_v4().as_bytes(), &session, &|| Ok(()))
            .is_err());
        assert!(staged
            .seals
            .iter()
            .all(|seal| seal.check_owner(&mut || Ok(())).is_ok()));
    }
    #[test]
    fn a_same_bytes_foreign_marker_cannot_inherit_the_held_profile_inventory_identity() {
        let f = Fixture::new();
        let staged = stage(&f, 8);
        let session = f.runtime().begin().unwrap();
        let capture =
            Capture::new(&staged.scene, &staged.seals, &session, limits(), &|| Ok(())).unwrap();
        let id = *uuid::Uuid::new_v4().as_bytes();
        capture.publish(id, &session, &|| Ok(())).unwrap();
        let bytes = marker_bytes(&session, id);
        let path = f.base.join(format!(
            "state/native-sources-v1/recovery-v3-{}.pbr",
            uuid::Uuid::from_bytes(id)
        ));
        let retained = path.with_file_name("retained-marker.pbr");
        fs::rename(&path, &retained).unwrap();
        fs::write(&path, &bytes).unwrap();
        assert!(load(&session, id, limits(), &|| Ok(())).is_err());
        assert_eq!(fs::read(&path).unwrap(), bytes);
        assert!(retained.is_file());
    }
    #[test]
    fn registered_checkpoint_keeps_selected_owners_after_live_scene_and_capture_drop() {
        let mut f = Fixture::new();
        let staged = stage(&f, 16);
        let session = f.runtime().begin().unwrap();
        let physical = staged.seals[0].store();
        let weak = physical.downgrade();
        let identity = physical.identity();
        let selected = staged.seals[0].reference();
        let capture =
            Capture::new(&staged.scene, &staged.seals, &session, limits(), &|| Ok(())).unwrap();
        let id = *uuid::Uuid::new_v4().as_bytes();
        capture.publish(id, &session, &|| Ok(())).unwrap();
        assert_eq!(
            session
                .checkpoint_census(&|| Ok(()))
                .unwrap()
                .registered_count(),
            1
        );
        drop((capture, physical, staged));
        assert!(weak.upgrade().is_some());
        let restored = session.resolve_store(identity, selected).unwrap();
        drop(restored);
        assert_eq!(
            session
                .checkpoint_census(&|| Ok(()))
                .unwrap()
                .registered_count(),
            1
        );
        drop(session);
        let runtime = f.runtime.take().unwrap();
        drop(runtime);
        assert!(weak.upgrade().is_none());
        assert!(f
            .connection
            .as_ref()
            .unwrap()
            .state_dir
            .join("native-sources-v1")
            .read_dir()
            .unwrap()
            .next()
            .is_some());
    }
    #[test]
    fn only_complete_load_can_register_an_independently_published_marker() {
        let f = Fixture::new();
        let staged = stage(&f, 8);
        let session = f.runtime().begin().unwrap();
        let capture =
            Capture::new(&staged.scene, &staged.seals, &session, limits(), &|| Ok(())).unwrap();
        let id = *uuid::Uuid::new_v4().as_bytes();
        capture.publish(id, &session, &|| Ok(())).unwrap();
        let copied = rewritten(&session, marker_bytes(&session, id), |_| {});
        assert!(session.checkpoint_census(&|| Ok(())).is_err());
        let decoded = load(&session, copied, limits(), &|| Ok(())).unwrap();
        assert_eq!(
            session
                .checkpoint_census(&|| Ok(()))
                .unwrap()
                .registered_count(),
            2
        );
        drop(decoded);
        let decoded = load(&session, copied, limits(), &|| Ok(())).unwrap();
        assert_eq!(
            session
                .checkpoint_census(&|| Ok(()))
                .unwrap()
                .registered_count(),
            2
        );
        drop(decoded);
    }
    #[test]
    fn checkpoint_census_preserves_pending_and_corrupt_marker_bytes_and_source_debt() {
        for corrupt in [false, true] {
            let f = Fixture::new();
            let staged = stage(&f, 16);
            let session = f.runtime().begin().unwrap();
            let capture =
                Capture::new(&staged.scene, &staged.seals, &session, limits(), &|| Ok(())).unwrap();
            let id = *uuid::Uuid::new_v4().as_bytes();
            capture.publish(id, &session, &|| Ok(())).unwrap();
            assert_eq!(
                session
                    .checkpoint_census(&|| Ok(()))
                    .unwrap()
                    .registered_count(),
                1
            );
            if corrupt {
                session.corrupt_checkpoint_for_test(id, 112).unwrap();
                // Restored mtime keeps cheap owner checks satisfied; full SHA must refuse.
                session.check().unwrap();
            } else {
                let pending = *uuid::Uuid::new_v4().as_bytes();
                assert!(session
                    .publish_recovery_marker(
                        pending,
                        8,
                        [3; 32],
                        &mut |writer| {
                            writer.write_all(&[4; 3]).map_err(|e| e.to_string())?;
                            Err("Injected incomplete marker".into())
                        },
                        &|| Ok(())
                    )
                    .is_err());
            }
            let retained = session.disk().used().unwrap();
            assert!(session.checkpoint_census(&|| Ok(())).is_err());
            assert_eq!(session.disk().used().unwrap(), retained);
            assert!(staged.seals[0].store().publication_marked());
        }
    }
    #[test]
    fn checkpoint_census_callbacks_can_publish_but_registration_race_refuses_observation() {
        let f = Fixture::new();
        let staged = stage(&f, 8);
        let session = f.runtime().begin().unwrap();
        let capture =
            Capture::new(&staged.scene, &staged.seals, &session, limits(), &|| Ok(())).unwrap();
        capture
            .publish(*uuid::Uuid::new_v4().as_bytes(), &session, &|| Ok(()))
            .unwrap();
        let calls = std::cell::Cell::new(0);
        let inserted = std::cell::Cell::new(false);
        let result = session.checkpoint_census(&|| {
            calls.set(calls.get() + 1);
            if calls.get() >= 2 && !inserted.replace(true) {
                capture
                    .publish(*uuid::Uuid::new_v4().as_bytes(), &session, &|| Ok(()))
                    .unwrap();
            }
            Ok(())
        });
        assert!(inserted.get());
        assert!(result.is_err());
        assert_eq!(
            session
                .checkpoint_census(&|| Ok(()))
                .unwrap()
                .registered_count(),
            2
        );
    }
    #[test]
    fn checkpoint_census_refuses_foreign_child_without_removing_owned_sources_or_debt() {
        let f = Fixture::new();
        let staged = stage(&f, 8);
        let session = f.runtime().begin().unwrap();
        let capture =
            Capture::new(&staged.scene, &staged.seals, &session, limits(), &|| Ok(())).unwrap();
        capture
            .publish(*uuid::Uuid::new_v4().as_bytes(), &session, &|| Ok(()))
            .unwrap();
        assert_eq!(
            session
                .checkpoint_census(&|| Ok(()))
                .unwrap()
                .registered_count(),
            1
        );
        let source_root = f
            .connection
            .as_ref()
            .unwrap()
            .state_dir
            .join("native-sources-v1");
        let child = fs::read_dir(source_root)
            .unwrap()
            .map(|entry| entry.unwrap())
            .find(|entry| {
                entry
                    .file_name()
                    .to_str()
                    .is_some_and(|name| name.starts_with("source-"))
            })
            .unwrap()
            .path();
        let foreign = child.join("foreign-checkpoint");
        fs::write(&foreign, b"retain").unwrap();
        let debt = session.disk().used().unwrap();
        assert!(session.checkpoint_census(&|| Ok(())).is_err());
        assert_eq!(fs::read(&foreign).unwrap(), b"retain");
        assert!(child.join("blocks.pbt").is_file());
        assert_eq!(session.disk().used().unwrap(), debt);
    }
}
