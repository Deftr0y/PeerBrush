//! Native backing recovery is separate from PSD/Source36 standard-data proof.
//! Framing/source proof alone grants no editable workspace registration.
use crate::{
    engine::scene::{Document36, PendingScene, RasterRoster, SceneSnapshot, StagedScene},
    large_document::{BlockRef, BufferedWrite, CommitRef, Error, ScratchBytes, Snapshot},
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
    pub(crate) fn from_native(
        scene: &SceneSnapshot,
        session: &SourceSession,
        limits: Limits,
        check: &dyn Fn() -> Result<()>,
    ) -> Result<Self> {
        limits.validate()?;
        let sealed = crate::engine::scene::seal_native_sources(
            scene,
            session,
            limits.refs,
            limits.stores,
            check,
        )?;
        Self::new_with_rasters(
            scene,
            &sealed.rasters,
            &sealed.seals,
            session,
            limits,
            check,
        )
    }
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
        Self::new_with_rasters(scene, scene.rasters(), seals, session, limits, check)
    }
    fn new_with_rasters(
        scene: &SceneSnapshot,
        sources: &RasterRoster,
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
            sources.get(reference)?;
            Ok(())
        })?;
        if count != sources.len() {
            return Err("Captured recovery metadata omits a raster source".into());
        }
        drop(typed);
        drop(raw);
        drop(raw_charge);
        let mut refs = 0usize;
        for (_, raster) in sources.iter() {
            check()?;
            refs = refs
                .checked_add(raster.physical_tiles().count())
                .filter(|n| *n <= limits.refs)
                .ok_or("Native recovery reference limit exceeded")?;
        }
        let roster_len = sources.len();
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
        for (id, raster) in sources.iter() {
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
    pub(crate) fn encoded_len(&self) -> Result<u64> {
        let bytes = HEADER
            .checked_add(self.metadata.len() as u64)
            .and_then(|n| n.checked_add(self.tables.len() as u64 * 112))
            .and_then(|n| n.checked_add(self.rasters.len() as u64 * 20))
            .and_then(|n| {
                n.checked_add(
                    self.rasters
                        .iter()
                        .map(|r| r.tiles.len() as u64 * 16)
                        .sum::<u64>(),
                )
            })
            .and_then(|n| n.checked_add(32))
            .filter(|n| *n <= MAX_MARKER)
            .ok_or("Native recovery framing exceeds its declared bounds")?;
        Ok(bytes)
    }
    /// Publish one immutable frame; the autosave coordinator owns retention.
    pub(crate) fn publish(
        &self,
        snapshot: [u8; 16],
        session: &SourceSession,
        check: &dyn Fn() -> Result<()>,
    ) -> Result<()> {
        self.publish_with_completion(snapshot, session, check, check)
    }
    /// Source-generation checks end at publication. Completion still verifies
    /// the actual instance/service and selected immutable source owners.
    pub(crate) fn publish_with_completion(
        &self,
        snapshot: [u8; 16],
        session: &SourceSession,
        check: &dyn Fn() -> Result<()>,
        complete: &dyn Fn() -> Result<()>,
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
        let expected = self.encoded_len()? - 32;
        if counted.bytes != expected {
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
        let result = session.publish_recovery_marker_with_completion(
            snapshot,
            full.bytes,
            sha256,
            &mut |output| {
                let mut buffered = BufferedWrite::new(output, &mut scratch);
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
            complete,
        );
        let result = result.and_then(|_| {
            let mut marker = session.open_recovery_marker(snapshot)?;
            session.register_checkpoint(proof, &mut marker, complete)
        });
        if result.is_err() {
            self.failed.store(true, Ordering::Release);
        }
        result
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
        shared: Option<crate::server::Shared>,
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
            let mut connection = crate::server::start(shared.clone(), base.join("state")).unwrap();
            connection
                .instance_lock
                .as_mut()
                .unwrap()
                .stop_recovery_for_test();
            let runtime = Self::new_runtime(&connection);
            Self {
                base,
                held,
                runtime: Some(runtime),
                connection: Some(connection),
                shared: Some(shared),
            }
        }
        fn new_runtime(connection: &crate::server::Connection) -> Arc<SourceRuntime> {
            let context = PersistentRootContext::from_explicit_state(connection).unwrap();
            SourceRuntime::new(
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
            .unwrap()
        }
        fn runtime(&self) -> &Arc<SourceRuntime> {
            self.runtime.as_ref().unwrap()
        }
        fn workspace(&self) -> crate::workspace::Registry {
            let workspace = crate::workspace::attach(self.shared.as_ref().unwrap());
            workspace.lock().unwrap().source_runtime = Some(self.runtime().clone());
            workspace
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            drop(self.shared.take());
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
        engine
            .resident_state_mut()
            .expect("resident fixture")
            .document = Document::new_depth(512, 256, depth).unwrap();
        engine
            .resident_state_mut()
            .expect("resident fixture")
            .document
            .layers[0]
            .pixels
            .set16(2, 3, [12345, 23456, 34567, 45678]);
        engine
            .resident_state_mut()
            .expect("resident fixture")
            .document
            .layers[0]
            .pixels
            .set16(511, 255, [65535, 32768, 1, 65535]);
        let original = engine
            .resident_state()
            .expect("resident fixture")
            .document
            .layers[0]
            .pixels
            .clone();
        engine
            .resident_state_mut()
            .expect("resident fixture")
            .document
            .layers[0]
            .pixels
            .retained = Some(Box::new(crate::retained::Original {
            pixels: Box::new(original),
            matrix: crate::retained::IDENTITY,
            projective: None,
            warps: vec![],
        }));
        engine
            .resident_state_mut()
            .expect("resident fixture")
            .document
            .layers[0]
            .effects
            .push(crate::effects::Effect {
                id: crate::engine::id(),
                kind: "blur".into(),
                enabled: true,
                weight: 0.75,
                settings: Arc::new(serde_json::json!({"radius": 2.0})),
            });
        let mut mask = crate::raster::Raster::new_depth(512, 256, depth);
        mask.set16(3, 2, [32768; 4]);
        engine
            .resident_state_mut()
            .expect("resident fixture")
            .document
            .layers[0]
            .mask = Some(crate::engine::Mask {
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
        engine
            .resident_state_mut()
            .expect("resident fixture")
            .document
            .selection = Some(coverage.bounds);
        engine
            .resident_state_mut()
            .expect("resident fixture")
            .document
            .selection_coverage = Some(coverage.clone());
        engine
            .resident_state_mut()
            .expect("resident fixture")
            .document
            .selection_previous = Some(coverage);
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
    fn native_engine(f: &Fixture, depth: u16) -> Engine {
        let staged = stage(f, depth);
        let document = crate::engine::native::NativeDocument::from_pending(
            staged.scene.into_provisional_pending().unwrap(),
        )
        .unwrap();
        let mut engine = Engine::from_native(document, 8).unwrap();
        engine.saved_revision = u64::MAX;
        engine
    }
    fn install_autosave_document(f: &Fixture, depth: u16) -> crate::workspace::Registry {
        let workspace = f.workspace();
        let source = native_engine(f, depth);
        let document = source.native_document().unwrap().clone();
        let mut engine = f.shared.as_ref().unwrap().lock().unwrap();
        engine.authority =
            crate::engine::authority::DocumentAuthority::from_native(document, 40).unwrap();
        engine.saved_revision = u64::MAX;
        drop(engine);
        workspace
    }
    #[test]
    #[cfg(windows)]
    fn refused_orphan_cleanup_does_not_withhold_an_actual_native_checkpoint() {
        for depth in [8, 16] {
            let f = Fixture::new();
            let workspace = install_autosave_document(&f, depth);
            let shared = f.shared.as_ref().unwrap();
            let key = shared.lock().unwrap().native_document().unwrap().key();
            let history = shared.lock().unwrap().history_lengths();
            let session = f.runtime().begin().unwrap();
            let orphan = session.store_for_lineage([0x74; 16]).unwrap();
            let spec = crate::large_document::TileSpec::new(256, 256, depth).unwrap();
            let physical = orphan.filesystem_identity().1;
            let path = source_file_contents(&f)
                .into_iter()
                .find(|(_, (identity, _))| *identity == physical)
                .unwrap()
                .0
                .parent()
                .unwrap()
                .to_owned();
            assert!(orphan
                .put(spec, &vec![47; spec.bytes()], &mut || {
                    if std::fs::metadata(path.join("blocks.pbt")).unwrap().len() > 24 {
                        Err(crate::large_document::Error::Cancelled)
                    } else {
                        Ok(())
                    }
                })
                .is_err());
            let original = orphan.file_bytes_for_test().unwrap();
            drop((orphan, session));
            assert!(f
                .runtime()
                .begin_existing()
                .unwrap()
                .collect_sources(&|| Ok(()))
                .is_err());
            let charged = f.runtime().disk().used().unwrap();
            let mut versions = crate::recovery::Versions::new();
            crate::recovery::checkpoint(&workspace, &f.base, &mut versions);
            let catalog = crate::recovery::catalog_in(&workspace, &f.base, &|| Ok(())).unwrap();
            assert!(catalog.native.error().is_none());
            assert_eq!(catalog.native.entries().len(), 1);
            assert!(catalog.native.entries()[0].error.is_none());
            assert_eq!(shared.lock().unwrap().native_document().unwrap().key(), key);
            assert_eq!(shared.lock().unwrap().history_lengths(), history);
            f.runtime()
                .begin_existing()
                .unwrap()
                .cleanup_check()
                .unwrap();
            assert!(f.runtime().disk().used().unwrap() > charged);
            let session = f.runtime().begin_existing().unwrap();
            assert_eq!(
                session
                    .owned_file_bytes_for_test(&path.join("blocks.pbt"))
                    .unwrap(),
                original
            );
        }
    }
    fn autosave_name(f: &Fixture, name: &str) {
        let shared = f.shared.as_ref().unwrap();
        let (id, revision, layer) = {
            let engine = shared.lock().unwrap();
            let document = engine.native_document().unwrap();
            (
                document.metadata().id().to_owned(),
                document.metadata().revision(),
                document.metadata().layer(0).unwrap().id().to_owned(),
            )
        };
        crate::server::edit_shared(
            shared,
            "human",
            &[serde_json::json!({"op":"layer.update","layer":layer,"name":name})],
            Some(revision),
            None,
            "Autosave fixture edit",
            None,
            Some(&id),
        )
        .unwrap();
    }
    #[test]
    #[cfg(windows)]
    fn native_autosave_keeps_three_versions_deduplicates_and_cleans_saved_work() {
        for depth in [8, 16] {
            let f = Fixture::new();
            let workspace = install_autosave_document(&f, depth);
            let shared = f.shared.as_ref().unwrap();
            let mut versions = crate::recovery::Versions::new();
            let source = shared
                .lock()
                .unwrap()
                .capture_scene(None)
                .unwrap()
                .into_native()
                .unwrap();
            for revision in 0..5 {
                if revision != 0 {
                    autosave_name(&f, &format!("Revision {revision}"));
                }
                let before = {
                    let engine = shared.lock().unwrap();
                    (
                        engine.native_document().unwrap().key(),
                        engine.history_lengths(),
                    )
                };
                crate::recovery::checkpoint(&workspace, &f.base, &mut versions);
                let error = workspace.lock().unwrap().recovery_error.clone();
                assert!(error.is_none(), "{error:?}");
                let engine = shared.lock().unwrap();
                assert_eq!(engine.native_document().unwrap().key(), before.0);
                assert_eq!(engine.history_lengths(), before.1);
                assert_eq!(engine.saved_revision, u64::MAX);
                assert!(engine.path.is_none());
            }
            let catalog = crate::recovery::catalog_in(&workspace, &f.base, &|| Ok(())).unwrap();
            assert_eq!(
                catalog
                    .native
                    .entries()
                    .iter()
                    .map(|entry| entry.revision.unwrap())
                    .collect::<Vec<_>>(),
                [4, 3, 2]
            );
            assert!(catalog
                .native
                .entries()
                .iter()
                .all(|entry| entry.bit_depth == Some(depth) && entry.error.is_none()));
            let files = source_file_contents(&f);
            let disk = f.runtime().disk().used().unwrap();
            crate::recovery::checkpoint(&workspace, &f.base, &mut versions);
            assert_eq!(source_file_contents(&f), files);
            assert_eq!(f.runtime().disk().used().unwrap(), disk);
            let current = shared
                .lock()
                .unwrap()
                .capture_scene(None)
                .unwrap()
                .into_native()
                .unwrap();
            for ((_, before), (_, after)) in source.rasters().iter().zip(current.rasters().iter()) {
                let region = crate::raster::backed::Region::new([0, 0, 512, 256]).unwrap();
                let before = before.prepare_region(region, &mut || Ok(())).unwrap();
                let after = after.prepare_region(region, &mut || Ok(())).unwrap();
                for (x, y) in [(2, 3), (511, 255)] {
                    assert_eq!(before.pixel16(x, y).unwrap(), after.pixel16(x, y).unwrap());
                }
            }
            crate::server::save(shared, &f.base.join("Saved native work.psd")).unwrap();
            let saved_key = shared.lock().unwrap().native_document().unwrap().key();
            crate::recovery::checkpoint(&workspace, &f.base, &mut versions);
            assert!(crate::recovery::catalog_in(&workspace, &f.base, &|| Ok(()))
                .unwrap()
                .native
                .entries()
                .is_empty());
            assert_eq!(
                shared.lock().unwrap().native_document().unwrap().key(),
                saved_key
            );
            let session = f.runtime().begin_existing().unwrap();
            assert_eq!(
                session
                    .checkpoint_census(&|| Ok(()))
                    .unwrap()
                    .registered_count(),
                0
            );
            assert!(f.base.join("Saved native work.psd").is_file());
        }
    }
    #[test]
    #[cfg(windows)]
    fn busy_native_retention_retries_without_creating_unbounded_checkpoints_and_close_cleans() {
        let f = Fixture::new();
        let workspace = install_autosave_document(&f, 16);
        let mut versions = crate::recovery::Versions::new();
        for revision in 0..3 {
            if revision != 0 {
                autosave_name(&f, &format!("Earlier {revision}"));
            }
            crate::recovery::checkpoint(&workspace, &f.base, &mut versions);
        }
        let blocker = f.runtime().begin_existing().unwrap();
        autosave_name(&f, "Durable before pruning");
        crate::recovery::checkpoint(&workspace, &f.base, &mut versions);
        let catalog = crate::recovery::catalog_in(&workspace, &f.base, &|| Ok(())).unwrap();
        assert_eq!(catalog.native.entries().len(), 4);
        assert!(workspace
            .lock()
            .unwrap()
            .recovery_error
            .as_ref()
            .unwrap()
            .contains("checkpoint is durable"));
        let disk = f.runtime().disk().used().unwrap();
        crate::recovery::checkpoint(&workspace, &f.base, &mut versions);
        assert_eq!(f.runtime().disk().used().unwrap(), disk);
        autosave_name(&f, "Newer human work");
        crate::recovery::checkpoint(&workspace, &f.base, &mut versions);
        assert_eq!(f.runtime().disk().used().unwrap(), disk);
        assert_eq!(
            crate::recovery::catalog_in(&workspace, &f.base, &|| Ok(()))
                .unwrap()
                .native
                .entries()
                .len(),
            4
        );
        drop(blocker);
        crate::recovery::checkpoint(&workspace, &f.base, &mut versions);
        assert!(workspace.lock().unwrap().recovery_error.is_none());
        let catalog = crate::recovery::catalog_in(&workspace, &f.base, &|| Ok(())).unwrap();
        assert_eq!(
            catalog
                .native
                .entries()
                .iter()
                .map(|entry| entry.revision.unwrap())
                .collect::<Vec<_>>(),
            [4, 3, 2]
        );
        let (project, id, revision) = {
            let engine = f.shared.as_ref().unwrap().lock().unwrap();
            (
                engine.project_id.clone(),
                engine.native_document().unwrap().metadata().id().to_owned(),
                engine.native_document().unwrap().metadata().revision(),
            )
        };
        crate::workspace::close_in(&workspace, &project, &id, revision, true, "human").unwrap();
        crate::recovery::checkpoint(&workspace, &f.base, &mut versions);
        assert!(workspace.lock().unwrap().recovery_retired.is_empty());
        assert!(versions.native.is_empty());
        assert!(crate::recovery::catalog_in(&workspace, &f.base, &|| Ok(()))
            .unwrap()
            .native
            .entries()
            .is_empty());
    }
    #[test]
    #[cfg(windows)]
    fn native_autosave_protects_the_new_checkpoint_when_older_clocks_are_in_the_future() {
        let f = Fixture::new();
        let workspace = install_autosave_document(&f, 16);
        let scene = f
            .shared
            .as_ref()
            .unwrap()
            .lock()
            .unwrap()
            .capture_scene(None)
            .unwrap()
            .into_native()
            .unwrap();
        let session = f.runtime().begin_existing().unwrap();
        let id = *uuid::Uuid::new_v4().as_bytes();
        Capture::from_native(&scene, &session, limits(), &|| Ok(()))
            .unwrap()
            .publish(id, &session, &|| Ok(()))
            .unwrap();
        let bytes = marker_bytes(&session, id);
        for _ in 0..3 {
            rewritten(&session, bytes.clone(), |bytes| {
                bytes[80..88].copy_from_slice(&u64::MAX.to_be_bytes())
            });
        }
        session
            .discard_checkpoint(id, limits(), &|| Ok(()))
            .unwrap();
        autosave_name(&f, "Latest actual work");
        crate::recovery::checkpoint(&workspace, &f.base, &mut Default::default());
        let catalog = crate::recovery::catalog_in(&workspace, &f.base, &|| Ok(())).unwrap();
        assert_eq!(catalog.native.entries().len(), 3);
        assert_eq!(
            catalog
                .native
                .entries()
                .iter()
                .filter(|entry| entry.timestamp_ms == Some(u64::MAX))
                .count(),
            2
        );
        assert_eq!(
            catalog
                .native
                .entries()
                .iter()
                .filter(|entry| entry.revision == Some(1))
                .count(),
            1
        );
    }
    #[test]
    fn native_autosave_reports_unavailable_platforms_without_a_resident_copy() {
        if cfg!(windows) {
            return;
        }
        let f = Fixture::new();
        let workspace = install_autosave_document(&f, 16);
        let disk = f.runtime().disk().used().unwrap();
        let key = f
            .shared
            .as_ref()
            .unwrap()
            .lock()
            .unwrap()
            .native_document()
            .unwrap()
            .key();
        crate::recovery::checkpoint(&workspace, &f.base, &mut Default::default());
        assert!(workspace
            .lock()
            .unwrap()
            .recovery_error
            .as_ref()
            .unwrap()
            .contains("retention is unavailable"));
        assert_eq!(f.runtime().disk().used().unwrap(), disk);
        assert_eq!(
            f.shared
                .as_ref()
                .unwrap()
                .lock()
                .unwrap()
                .native_document()
                .unwrap()
                .key(),
            key
        );
        assert!(f
            .shared
            .as_ref()
            .unwrap()
            .lock()
            .unwrap()
            .resident_state()
            .is_err());
    }
    #[test]
    #[cfg(windows)]
    fn native_autosave_cancels_changed_work_and_checks_cached_service_identity() {
        let f = Fixture::new();
        let workspace = install_autosave_document(&f, 16);
        let shared = f.shared.as_ref().unwrap();
        let scene = shared
            .lock()
            .unwrap()
            .capture_scene(None)
            .unwrap()
            .into_native()
            .unwrap();
        let directory = f.base.join("state/native-sources-v1");
        let changed = std::cell::Cell::new(false);
        let operation = crate::server::FileOperation::begin_workspace(&workspace).unwrap();
        let result =
            crate::recovery::native::checkpoint(&workspace, &f.base, shared, &scene, None, &|| {
                assert!(shared.try_lock().is_ok());
                assert!(workspace.try_lock().is_ok());
                let has_bytes = fs::read_dir(&directory).unwrap().flatten().any(|entry| {
                    entry.file_name().to_string_lossy().starts_with("pending-")
                        && entry.metadata().unwrap().len() != 0
                });
                if has_bytes && !changed.replace(true) {
                    autosave_name(&f, "Later human work");
                }
                Ok(())
            });
        assert!(changed.get());
        assert!(result.err().unwrap().contains("source changed"));
        drop(operation);
        assert!(!fs::read_dir(&directory)
            .unwrap()
            .flatten()
            .any(|entry| entry.file_name().to_string_lossy().starts_with("pending-")));
        assert!(crate::recovery::catalog_in(&workspace, &f.base, &|| Ok(()))
            .unwrap()
            .native
            .entries()
            .is_empty());
        let mut versions = crate::recovery::Versions::new();
        crate::recovery::checkpoint(&workspace, &f.base, &mut versions);
        assert!(workspace.lock().unwrap().recovery_error.is_none());
        let files = source_file_contents(&f);
        let foreign = Fixture::new();
        workspace.lock().unwrap().source_runtime = Some(foreign.runtime().clone());
        crate::recovery::checkpoint(&workspace, &f.base, &mut versions);
        assert!(workspace
            .lock()
            .unwrap()
            .recovery_error
            .as_ref()
            .unwrap()
            .contains("source service changed"));
        assert!(!foreign.base.join("state/native-sources-v1").exists());
        assert_eq!(source_file_contents(&f), files);
        workspace.lock().unwrap().source_runtime = Some(f.runtime().clone());
        crate::recovery::checkpoint(&workspace, &f.base, &mut versions);
        assert!(workspace.lock().unwrap().recovery_error.is_none());
        assert_eq!(source_file_contents(&f), files);
    }
    #[test]
    #[cfg(windows)]
    fn a_human_edit_after_native_publication_keeps_the_durable_snapshot_and_next_tick() {
        let f = Fixture::new();
        let workspace = install_autosave_document(&f, 16);
        let shared = f.shared.as_ref().unwrap();
        let scene = shared
            .lock()
            .unwrap()
            .capture_scene(None)
            .unwrap()
            .into_native()
            .unwrap();
        let directory = f.base.join("state/native-sources-v1");
        let edited = std::cell::Cell::new(false);
        let operation = crate::server::FileOperation::begin_workspace(&workspace).unwrap();
        let result =
            crate::recovery::native::checkpoint(&workspace, &f.base, shared, &scene, None, &|| {
                let published = fs::read_dir(&directory).unwrap().flatten().any(|entry| {
                    entry
                        .file_name()
                        .to_string_lossy()
                        .starts_with("recovery-v3-")
                });
                if published && !edited.replace(true) {
                    autosave_name(&f, "Newer after publication");
                }
                Ok(())
            })
            .unwrap();
        drop(operation);
        assert!(edited.get());
        assert!(result.error.is_none());
        assert_eq!(result.saved.key, scene.key());
        assert_ne!(
            shared.lock().unwrap().native_document().unwrap().key(),
            scene.key()
        );
        let catalog = crate::recovery::catalog_in(&workspace, &f.base, &|| Ok(())).unwrap();
        assert!(catalog.native.error().is_none());
        assert_eq!(catalog.native.entries().len(), 1);
        assert_eq!(catalog.native.entries()[0].revision, Some(0));
        let mut versions = crate::recovery::Versions::new();
        versions
            .native
            .insert(scene.identity().project_id.clone(), result.saved);
        crate::recovery::checkpoint(&workspace, &f.base, &mut versions);
        assert!(workspace.lock().unwrap().recovery_error.is_none());
        let catalog = crate::recovery::catalog_in(&workspace, &f.base, &|| Ok(())).unwrap();
        assert_eq!(
            catalog
                .native
                .entries()
                .iter()
                .map(|entry| entry.revision.unwrap())
                .collect::<Vec<_>>(),
            [1, 0]
        );
        assert_eq!(shared.lock().unwrap().history_lengths(), [1, 0]);
    }
    #[test]
    #[cfg(windows)]
    fn native_autosave_defers_active_work_in_either_tab_and_retries_next_tick() {
        let f = Fixture::new();
        let workspace = install_autosave_document(&f, 16);
        let shared = f.shared.as_ref().unwrap();
        let mut versions = crate::recovery::Versions::new();
        crate::recovery::checkpoint(&workspace, &f.base, &mut versions);
        let project = shared.lock().unwrap().project_id.clone();
        let original = versions.native[&project].key;
        autosave_name(&f, "Newer foreground work");
        let active = crate::server::FileOperation::begin(shared).unwrap();
        crate::recovery::checkpoint(&workspace, &f.base, &mut versions);
        assert_eq!(versions.native[&project].key, original);
        assert!(workspace.lock().unwrap().recovery_error.is_none());
        drop(active);
        let other = crate::workspace::new_project(shared, 8, 8, 8, None).unwrap();
        let scene = other
            .lock()
            .unwrap()
            .capture_scene(None)
            .unwrap()
            .into_native()
            .unwrap();
        let edit = crate::workspace::native_dispatch::begin_edit_scene(&other, &scene).unwrap();
        crate::recovery::checkpoint(&workspace, &f.base, &mut versions);
        assert_eq!(versions.native[&project].key, original);
        assert!(workspace.lock().unwrap().recovery_error.is_none());
        drop(edit);
        crate::recovery::checkpoint(&workspace, &f.base, &mut versions);
        assert_ne!(versions.native[&project].key, original);
        assert!(workspace.lock().unwrap().recovery_error.is_none());
    }
    #[test]
    #[cfg(windows)]
    fn native_autosave_admission_and_unreadable_records_preserve_complete_versions() {
        let f = Fixture::new();
        let workspace = install_autosave_document(&f, 16);
        let mut versions = crate::recovery::Versions::new();
        let disk = f.runtime().disk().used().unwrap();
        let budget = f.runtime().resident();
        let ballast =
            MetadataCharge::reserve(&budget, budget.limit() - budget.used(), &|| Ok(())).unwrap();
        crate::recovery::checkpoint(&workspace, &f.base, &mut versions);
        assert!(workspace.lock().unwrap().recovery_error.is_some());
        assert_eq!(f.runtime().disk().used().unwrap(), disk);
        drop(ballast);
        crate::recovery::checkpoint(&workspace, &f.base, &mut versions);
        assert!(workspace.lock().unwrap().recovery_error.is_none());
        let catalog = crate::recovery::catalog_in(&workspace, &f.base, &|| Ok(())).unwrap();
        let original = uuid::Uuid::parse_str(
            catalog.native.entries()[0]
                .snapshot
                .strip_prefix("native:")
                .unwrap(),
        )
        .unwrap();
        let session = f.runtime().begin_existing().unwrap();
        let bytes = marker_bytes(&session, *original.as_bytes());
        let corrupt = rewritten(&session, bytes.clone(), |bytes| bytes[19] = 99);
        let corrupt_bytes = marker_bytes(&session, corrupt);
        drop(session);
        autosave_name(&f, "Must retain earlier work");
        crate::recovery::checkpoint(&workspace, &f.base, &mut versions);
        assert!(workspace
            .lock()
            .unwrap()
            .recovery_error
            .as_ref()
            .unwrap()
            .contains("inventory is incomplete"));
        let session = f.runtime().begin_existing().unwrap();
        assert_eq!(marker_bytes(&session, *original.as_bytes()), bytes);
        assert_eq!(marker_bytes(&session, corrupt), corrupt_bytes);
        assert_eq!(session.recovery_marker_ids().unwrap().iter().count(), 2);
    }
    #[test]
    #[cfg(windows)]
    fn real_periodic_worker_publishes_a_restorable_native_checkpoint_and_joins() {
        let f = Fixture::new();
        let workspace = install_autosave_document(&f, 16);
        let shared = f.shared.as_ref().unwrap();
        let source = shared
            .lock()
            .unwrap()
            .capture_scene(None)
            .unwrap()
            .into_native()
            .unwrap();
        let mut worker = crate::recovery::Worker::start(workspace.clone(), f.base.clone()).unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
        let mut ready = false;
        while std::time::Instant::now() < deadline {
            ready = f
                .runtime()
                .begin_existing()
                .and_then(|session| session.recovery_marker_ids())
                .is_ok_and(|ids| ids.iter().next().is_some());
            if ready {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        worker.stop();
        assert!(
            ready,
            "Periodic native autosave did not publish within its deadline"
        );
        assert_eq!(
            workspace
                .lock()
                .unwrap()
                .file_operations
                .load(std::sync::atomic::Ordering::Acquire),
            0
        );
        assert_eq!(
            shared.lock().unwrap().native_document().unwrap().key(),
            source.key()
        );
        assert_eq!(shared.lock().unwrap().history_lengths(), [0, 0]);
        let catalog = crate::recovery::catalog_in(&workspace, &f.base, &|| Ok(())).unwrap();
        assert_eq!(catalog.native.entries().len(), 1);
        let restored = crate::recovery::restore(
            &workspace,
            &f.base,
            &catalog.native.entries()[0].snapshot,
            &Default::default(),
            None,
        )
        .unwrap();
        let restored = restored
            .lock()
            .unwrap()
            .capture_scene(None)
            .unwrap()
            .into_native()
            .unwrap();
        for ((_, before), (_, after)) in source.rasters().iter().zip(restored.rasters().iter()) {
            let region = crate::raster::backed::Region::new([0, 0, 512, 256]).unwrap();
            let before = before.prepare_region(region, &mut || Ok(())).unwrap();
            let after = after.prepare_region(region, &mut || Ok(())).unwrap();
            for (x, y) in [(2, 3), (511, 255)] {
                assert_eq!(before.pixel16(x, y).unwrap(), after.pixel16(x, y).unwrap());
            }
        }
    }
    fn commit(engine: &mut Engine, session: &SourceSession, commands: &[serde_json::Value]) {
        let prepared = crate::engine::native::worker::prepare(
            crate::engine::native::worker::tests::capture(&engine.authority, commands),
            session,
            &|| Ok(()),
        )
        .unwrap();
        let (base, history, delta) = prepared.into_parts();
        let commit = history
            .prepare_commit(
                &base,
                delta,
                crate::engine::native::HistoryRequest {
                    actor: "human",
                    label: "Recovery fixture",
                    task: None,
                    gesture: None,
                },
                &|| Ok(()),
            )
            .unwrap();
        engine
            .authority
            .commit_native(
                crate::engine::native::Publication {
                    actor: "human",
                    leases: &[],
                    now: 0,
                    closed: false,
                    write_available: true,
                },
                commit,
            )
            .unwrap()
            .retire(session, &|| session.cleanup_check())
            .unwrap();
    }
    #[test]
    fn live_native_capture_preserves_exact_metadata_samples_and_engine_history() {
        for depth in [8, 16] {
            let f = Fixture::new();
            let mut engine = native_engine(&f, depth);
            let session = f.runtime().begin().unwrap();
            let layer = engine.authority.view().layer(0).unwrap().id().to_owned();
            commit(
                &mut engine,
                &session,
                &[serde_json::json!({"op":"layer.update","layer":layer,"visible":false})],
            );
            let key = engine.native_document().unwrap().key();
            let scene = engine.capture_scene(None).unwrap().into_native().unwrap();
            let capture = Capture::from_native(&scene, &session, limits(), &|| Ok(())).unwrap();
            let id = *uuid::Uuid::new_v4().as_bytes();
            capture.publish(id, &session, &|| Ok(())).unwrap();
            let decoded = load(&session, id, limits(), &|| Ok(())).unwrap();
            let before =
                Canonical::from_serializable(&scene.metadata(), limits().metadata, &|| Ok(()))
                    .unwrap();
            let after = Canonical::from_serializable(
                decoded.pending.metadata(),
                limits().metadata,
                &|| Ok(()),
            )
            .unwrap();
            assert_eq!(before.bytes(), after.bytes());
            assert_eq!(decoded.project, capture.project);
            assert_eq!(decoded.marker.snapshot(), id);
            assert_eq!(scene.rasters().len(), decoded.pending.rasters().len());
            for ((id, original), (restored_id, restored)) in
                scene.rasters().iter().zip(decoded.pending.rasters().iter())
            {
                assert_eq!(id, restored_id);
                assert_eq!(original.depth(), restored.depth());
                let region = crate::raster::backed::Region::new([0, 0, 512, 256]).unwrap();
                let original = original.prepare_region(region, &mut || Ok(())).unwrap();
                let restored = restored.prepare_region(region, &mut || Ok(())).unwrap();
                for y in 0..256 {
                    for x in 0..512 {
                        assert_eq!(
                            original.pixel16(x, y).unwrap(),
                            restored.pixel16(x, y).unwrap()
                        );
                    }
                }
            }
            assert!(engine.resident_state().is_err());
            assert_eq!(engine.native_document().unwrap().key(), key);
            assert_eq!(engine.history_lengths(), [1, 0]);
            assert_eq!(engine.saved_revision, u64::MAX);
        }
    }
    #[test]
    fn native_zero_and_newly_painted_sources_seal_without_a_resident_stage() {
        for depth in [8, 16] {
            let f = Fixture::new();
            let session = f.runtime().begin().unwrap();
            let document = crate::engine::native::NativeDocument::new_blank(
                4,
                2,
                depth,
                &session.resident(),
                &session.disk(),
                &|| Ok(()),
            )
            .unwrap();
            let mut engine = Engine::from_native(document, 8).unwrap();
            let layer = engine.authority.view().layer(0).unwrap().id().to_owned();
            for painted in [false, true] {
                if painted {
                    commit(
                        &mut engine,
                        &session,
                        &[
                            serde_json::json!({"op":"paint.fill","layer":layer,"rect":[0,0,4,2],"color":[39,101,233,255]}),
                        ],
                    );
                }
                let scene = engine.capture_scene(None).unwrap().into_native().unwrap();
                let capture = Capture::from_native(&scene, &session, limits(), &|| Ok(())).unwrap();
                assert_eq!(capture.tables.is_empty(), !painted);
                let id = *uuid::Uuid::new_v4().as_bytes();
                capture.publish(id, &session, &|| Ok(())).unwrap();
                let decoded = load(&session, id, limits(), &|| Ok(())).unwrap();
                let original = scene.rasters().iter().next().unwrap().1;
                let restored = decoded.pending.rasters().iter().next().unwrap().1;
                let region = crate::raster::backed::Region::new([0, 0, 4, 2]).unwrap();
                let original = original.prepare_region(region, &mut || Ok(())).unwrap();
                let restored = restored.prepare_region(region, &mut || Ok(())).unwrap();
                for y in 0..2 {
                    for x in 0..4 {
                        let expected = if painted {
                            [39 * 257, 101 * 257, 233 * 257, 65535]
                        } else {
                            [0; 4]
                        };
                        assert_eq!(original.pixel16(x, y).unwrap(), expected);
                        assert_eq!(restored.pixel16(x, y).unwrap(), expected);
                    }
                }
                assert!(engine.resident_state().is_err());
                assert_eq!(engine.native_document().unwrap().key(), scene.key());
                assert_eq!(engine.history_lengths(), [usize::from(painted), 0]);
                assert_eq!(engine.saved_revision, 0);
            }
        }
    }
    #[test]
    fn native_capture_refuses_pending_foreign_cancelled_and_over_limit_sources() {
        let f = Fixture::new();
        let staged = stage(&f, 16);
        let session = f.runtime().begin().unwrap();
        assert!(Capture::from_native(&staged.scene, &session, limits(), &|| Ok(())).is_err());
        let engine = native_engine(&f, 16);
        let scene = engine.capture_scene(None).unwrap().into_native().unwrap();
        let key = engine.native_document().unwrap().key();
        let disk = session.disk().used().unwrap();
        let resident = session.resident().used();
        let foreign = Fixture::new();
        let foreign_session = foreign.runtime().begin().unwrap();
        assert!(Capture::from_native(&scene, &foreign_session, limits(), &|| Ok(())).is_err());
        assert!(
            Capture::from_native(&scene, &session, limits(), &|| Err("Cancelled".into())).is_err()
        );
        assert!(Capture::from_native(
            &scene,
            &session,
            Limits {
                refs: 1,
                ..limits()
            },
            &|| Ok(())
        )
        .is_err());
        assert!(Capture::from_native(
            &scene,
            &session,
            Limits {
                stores: 0,
                ..limits()
            },
            &|| Ok(())
        )
        .is_err());
        assert_eq!(session.disk().used().unwrap(), disk);
        assert_eq!(session.resident().used(), resident);
        assert_eq!(engine.native_document().unwrap().key(), key);
        assert_eq!(engine.history_lengths(), [0, 0]);
        assert_eq!(engine.saved_revision, u64::MAX);
        assert_eq!(
            session
                .checkpoint_census(&|| Ok(()))
                .unwrap()
                .registered_count(),
            0
        );
        // Refusal keeps every source usable for a subsequent valid capture.
        assert!(Capture::from_native(&scene, &session, limits(), &|| Ok(())).is_ok());
    }
    #[test]
    fn shared_restore_registers_distinct_unsaved_native_tabs_and_keeps_every_source() {
        for depth in [8, 16] {
            let f = Fixture::new();
            let workspace = f.workspace();
            let initiating = f
                .shared
                .as_ref()
                .unwrap()
                .lock()
                .unwrap()
                .project_id
                .clone();
            let original_root = f
                .shared
                .as_ref()
                .unwrap()
                .lock()
                .unwrap()
                .resident_state()
                .unwrap()
                .document
                .id
                .clone();
            let engine = native_engine(&f, depth);
            let scene = engine.capture_scene(None).unwrap().into_native().unwrap();
            let session = f.runtime().begin().unwrap();
            let capture = Capture::from_native(&scene, &session, limits(), &|| Ok(())).unwrap();
            let id = uuid::Uuid::new_v4();
            capture
                .publish(*id.as_bytes(), &session, &|| Ok(()))
                .unwrap();
            let bytes = marker_bytes(&session, *id.as_bytes());
            let disk = session.disk().used().unwrap();
            let first = crate::recovery::restore(
                &workspace,
                &f.base,
                &id.to_string(),
                &Default::default(),
                Some(&initiating),
            )
            .unwrap();
            let first_project = first.lock().unwrap().project_id.clone();
            let second = crate::recovery::restore(
                &workspace,
                &f.base,
                &id.to_string(),
                &Default::default(),
                Some(&initiating),
            )
            .unwrap();
            for restored in [&first, &second] {
                let e = restored.lock().unwrap();
                assert!(e.resident_state().is_err());
                assert!(e.path.is_none());
                assert_eq!(e.saved_revision, u64::MAX);
                assert_eq!(e.history_lengths(), [0, 0]);
                let native = e.native_document().unwrap();
                assert_ne!(native.metadata().id(), scene.identity().document_id);
                assert_eq!(native.metadata().depth(), depth);
                let restored_scene = e.capture_scene(None).unwrap().into_native().unwrap();
                let mut original = serde_json::to_value(scene.metadata()).unwrap();
                original["id"] = serde_json::json!(native.metadata().id());
                assert_eq!(
                    original,
                    serde_json::to_value(restored_scene.metadata()).unwrap()
                );
                drop(e);
                for ((_, before), (_, after)) in
                    scene.rasters().iter().zip(restored_scene.rasters().iter())
                {
                    let region = crate::raster::backed::Region::new([0, 0, 512, 256]).unwrap();
                    let before = before.prepare_region(region, &mut || Ok(())).unwrap();
                    let after = after.prepare_region(region, &mut || Ok(())).unwrap();
                    for y in 0..256 {
                        for x in 0..512 {
                            assert_eq!(before.pixel16(x, y).unwrap(), after.pixel16(x, y).unwrap());
                        }
                    }
                }
            }
            assert_ne!(
                first
                    .lock()
                    .unwrap()
                    .native_document()
                    .unwrap()
                    .metadata()
                    .id(),
                second
                    .lock()
                    .unwrap()
                    .native_document()
                    .unwrap()
                    .metadata()
                    .id()
            );
            assert_eq!(crate::workspace::entries_in(&workspace).len(), 3);
            assert_eq!(
                crate::workspace::active_id(f.shared.as_ref().unwrap()),
                first_project
            );
            assert_eq!(
                f.shared
                    .as_ref()
                    .unwrap()
                    .lock()
                    .unwrap()
                    .resident_state()
                    .unwrap()
                    .document
                    .id,
                original_root
            );
            assert_eq!(marker_bytes(&session, *id.as_bytes()), bytes);
            assert_eq!(session.disk().used().unwrap(), disk);
        }
    }
    #[test]
    fn native_recovery_restores_after_reopening_the_real_source_inventory() {
        for depth in [8, 16] {
            let mut f = Fixture::new();
            let workspace = f.workspace();
            let id = uuid::Uuid::new_v4();
            #[cfg(windows)]
            let retired = uuid::Uuid::new_v4();
            let expected;
            {
                let engine = native_engine(&f, depth);
                let scene = engine.capture_scene(None).unwrap().into_native().unwrap();
                let session = f.runtime().begin().unwrap();
                let capture = Capture::from_native(&scene, &session, limits(), &|| Ok(())).unwrap();
                capture
                    .publish(*id.as_bytes(), &session, &|| Ok(()))
                    .unwrap();
                #[cfg(windows)]
                capture
                    .publish(*retired.as_bytes(), &session, &|| Ok(()))
                    .unwrap();
                let raster = scene.rasters().iter().next().unwrap().1;
                let region = crate::raster::backed::Region::new([0, 0, 4, 4]).unwrap();
                expected = raster
                    .prepare_region(region, &mut || Ok(()))
                    .unwrap()
                    .pixel16(2, 3)
                    .unwrap();
            }
            #[cfg(windows)]
            f.runtime()
                .begin_existing()
                .unwrap()
                .discard_checkpoint(*retired.as_bytes(), limits(), &|| Ok(()))
                .unwrap();
            workspace.lock().unwrap().source_runtime = None;
            drop(f.runtime.take());
            let runtime = Fixture::new_runtime(f.connection.as_ref().unwrap());
            workspace.lock().unwrap().source_runtime = Some(runtime.clone());
            f.runtime = Some(runtime);
            let restored = crate::recovery::restore(
                &workspace,
                &f.base,
                &id.to_string(),
                &Default::default(),
                None,
            )
            .unwrap();
            let scene = restored
                .lock()
                .unwrap()
                .capture_scene(None)
                .unwrap()
                .into_native()
                .unwrap();
            assert_eq!(scene.metadata().depth(), depth);
            let raster = scene.rasters().iter().next().unwrap().1;
            let region = crate::raster::backed::Region::new([0, 0, 4, 4]).unwrap();
            assert_eq!(
                raster
                    .prepare_region(region, &mut || Ok(()))
                    .unwrap()
                    .pixel16(2, 3)
                    .unwrap(),
                expected
            );
            let session = f.runtime().begin_existing().unwrap();
            #[cfg(windows)]
            assert!(session.open_recovery_marker(*retired.as_bytes()).is_err());
            assert!(session.open_recovery_marker(*id.as_bytes()).is_ok());
            assert_eq!(
                session
                    .checkpoint_census(&|| Ok(()))
                    .unwrap()
                    .registered_count(),
                1
            );
        }
    }
    struct RefusedMirror<'a> {
        session: &'a SourceSession,
        original: Store,
        alias: bool,
        cancelled: std::cell::Cell<bool>,
    }
    impl StageSession for RefusedMirror<'_> {
        fn check(&self) -> Result<()> {
            if self.cancelled.get() {
                Err("cancel after mirror admission".into())
            } else {
                self.session.check()
            }
        }
        fn store_for_lineage(&self, lineage: [u8; 16]) -> Result<Store> {
            if self.alias {
                Ok(self.original.clone())
            } else {
                let mirror = self.session.store_for_lineage(lineage)?;
                self.cancelled.set(true);
                Ok(mirror)
            }
        }
        fn seal_store(
            &self,
            store: &Store,
            refs: &[BlockRef],
            metadata: &[u8],
        ) -> Result<Snapshot> {
            self.session.seal_store(store, refs, metadata)
        }
        fn try_seal_store(
            &self,
            store: &Store,
            refs: &[BlockRef],
            metadata: &[u8],
        ) -> Result<Option<Snapshot>> {
            self.session.try_seal_store(store, refs, metadata)
        }
    }
    #[test]
    fn reopened_catalog_selects_every_shared_prefix_and_can_recheckpoint_the_middle_version() {
        for depth in [8, 16] {
            let mut f = Fixture::new();
            let workspace = f.workspace();
            let ids = [[0x11; 16], [0x22; 16], [0x33; 16]];
            let mut expected = Vec::new();
            let mut selections = Vec::new();
            let mut physical = None;
            {
                let mut resident = Engine::from_document(Document::new_depth(4, 4, depth).unwrap());
                let cache = LegacyTileCache::new(&f.runtime().resident(), 8).unwrap();
                for (at, id) in ids.into_iter().enumerate() {
                    let document = &mut resident.resident_state_mut().unwrap().document;
                    document.layers[0].pixels.set16(
                        2,
                        3,
                        [
                            12347 + at as u16 * 1000,
                            23459 + at as u16 * 2000,
                            34571 + at as u16 * 3000,
                            if at == 0 { 0 } else { 65535 },
                        ],
                    );
                    document.revision = at as u64;
                    expected.push(document.layers[0].pixels.get16(2, 3));
                    let captured = resident.capture_scene(None).unwrap();
                    let staged = captured
                        .stage(&Runtime(f.runtime().clone()), &cache, &|| Ok(()))
                        .unwrap();
                    let selected = staged.seals[0].reference();
                    selections.push(selected);
                    let owner = staged.seals[0].store().identity();
                    if let Some(previous) = physical {
                        assert_eq!(owner, previous);
                    } else {
                        physical = Some(owner);
                    }
                    let native = crate::engine::native::NativeDocument::from_pending(
                        staged.scene.into_provisional_pending().unwrap(),
                    )
                    .unwrap();
                    let engine = Engine::from_native(native, 8).unwrap();
                    let scene = engine.capture_scene(None).unwrap().into_native().unwrap();
                    let session = f.runtime().begin_existing().unwrap();
                    Capture::from_native(&scene, &session, limits(), &|| Ok(()))
                        .unwrap()
                        .publish(id, &session, &|| Ok(()))
                        .unwrap();
                }
            }
            assert_ne!(selections[0], selections[1]);
            assert_ne!(selections[1], selections[2]);
            workspace.lock().unwrap().source_runtime = None;
            drop(f.runtime.take());
            let runtime = Fixture::new_runtime(f.connection.as_ref().unwrap());
            workspace.lock().unwrap().source_runtime = Some(runtime.clone());
            f.runtime = Some(runtime);
            let catalog = crate::recovery::catalog_in(&workspace, &f.base, &|| Ok(())).unwrap();
            assert!(catalog.native.error().is_none());
            assert_eq!(catalog.native.entries().len(), 3);
            assert!(
                catalog
                    .native
                    .entries()
                    .iter()
                    .all(|entry| entry.error.is_none()),
                "{:?}",
                catalog
                    .native
                    .entries()
                    .iter()
                    .map(|entry| &entry.error)
                    .collect::<Vec<_>>()
            );
            let session = f.runtime().begin_existing().unwrap();
            let mut metadata = None;
            for at in [2, 0, 1] {
                let decoded = load(&session, ids[at], limits(), &|| Ok(())).unwrap();
                let store = decoded.seals[0].store();
                assert_eq!(store.checkpoint_reference().unwrap(), Some(selections[0]));
                let raster = decoded.pending.rasters().iter().next().unwrap().1;
                let region = crate::raster::backed::Region::new([0, 0, 4, 4]).unwrap();
                assert_eq!(
                    raster
                        .prepare_region(region, &mut || Ok(()))
                        .unwrap()
                        .pixel16(2, 3)
                        .unwrap(),
                    expected[at]
                );
                if at == 1 {
                    metadata = Some(decoded.pending);
                }
            }
            let mut union = Vec::new();
            for selected in &selections[..2] {
                let source = session.resolve_store(physical.unwrap(), *selected).unwrap();
                source
                    .visit_blocks(&mut || Ok(()), |reference| {
                        union.push(reference);
                        Ok(())
                    })
                    .unwrap();
            }
            let store = session
                .resolve_store(physical.unwrap(), selections[0])
                .unwrap()
                .store();
            let unchanged = store.file_bytes_for_test().unwrap();
            assert!(session
                .seal_store(&store, &union, b"unsupported readonly union")
                .is_err());
            assert_eq!(store.file_bytes_for_test().unwrap(), unchanged);
            let mut selected_refs = Vec::new();
            for selected in &selections[..2] {
                let source = session.resolve_store(physical.unwrap(), *selected).unwrap();
                let mut refs = Vec::new();
                source
                    .visit_blocks(&mut || Ok(()), |reference| {
                        refs.push(reference);
                        Ok(())
                    })
                    .unwrap();
                assert_eq!(refs.len(), 1);
                selected_refs.push(refs[0]);
            }
            for across_layers in [false, true] {
                let owner =
                    MetadataCharge::reserve(&session.resident(), 65536, &|| Ok(())).unwrap();
                let mut typed: Document36 =
                    crate::engine::scene::wire_copy(metadata.as_ref().unwrap().metadata());
                typed.width = 512;
                typed.height = 256;
                typed.layers[0].pixels.width = 512;
                typed.layers[0].pixels.height = 256;
                let make_surface = |refs| {
                    Surface::from_plane(
                        crate::raster::backed::Plane::from_blocks(
                            512,
                            256,
                            depth,
                            store.clone(),
                            refs,
                        )
                        .unwrap(),
                    )
                    .unwrap()
                };
                let mut surfaces = vec![make_surface(std::collections::BTreeMap::from([(
                    (0, 0),
                    selected_refs[0],
                )]))];
                if across_layers {
                    let mut second = crate::engine::scene::wire_copy(&typed.layers[0]);
                    second.id = crate::engine::id();
                    second.pixels.id = 2;
                    typed.layers.push(second);
                    surfaces.push(make_surface(std::collections::BTreeMap::from([(
                        (1, 0),
                        selected_refs[1],
                    )])));
                } else {
                    surfaces[0] = make_surface(std::collections::BTreeMap::from([
                        ((0, 0), selected_refs[0]),
                        ((1, 0), selected_refs[1]),
                    ]));
                }
                let pending = PendingScene::new(
                    crate::psd::memory36::Admitted::from_parts(Arc::new(typed), owner),
                    RasterRoster::new(surfaces).unwrap(),
                    session.resident(),
                    session.disk(),
                )
                .unwrap();
                let native = crate::engine::native::NativeDocument::from_pending(pending).unwrap();
                let engine = Engine::from_native(native, 8).unwrap();
                let mixed = engine.capture_scene(None).unwrap().into_native().unwrap();
                let key = mixed.key();
                assert!(crate::engine::scene::seal_native_sources(
                    &mixed,
                    &session,
                    1024,
                    16,
                    &|| Err("cancel union".into()),
                )
                .is_err());
                for alias in [true, false] {
                    let refused = RefusedMirror {
                        session: &session,
                        original: store.clone(),
                        alias,
                        cancelled: std::cell::Cell::new(false),
                    };
                    let disk_before = session.disk().used().unwrap();
                    assert!(crate::engine::scene::source_copy::seal_sources(
                        mixed.rasters().as_slice(),
                        &session.resident(),
                        &session.disk(),
                        &refused,
                        b"refused mirror fixture",
                        (1024, 16),
                        0,
                        &|| Ok(()),
                    )
                    .is_err());
                    assert_eq!(store.file_bytes_for_test().unwrap(), unchanged);
                    assert!(session.disk().used().unwrap() >= disk_before);
                }
                let foreign = ResidentBudget::new(1024 * 1024).unwrap();
                assert!(crate::engine::scene::source_copy::seal_sources(
                    mixed.rasters().as_slice(),
                    &foreign,
                    &session.disk(),
                    &session,
                    b"foreign admission fixture",
                    (1024, 16),
                    0,
                    &|| Ok(()),
                )
                .is_err());
                let sealed =
                    crate::engine::scene::seal_native_sources(&mixed, &session, 1024, 16, &|| {
                        Ok(())
                    })
                    .unwrap();
                assert_eq!(sealed.seals.len(), 1);
                let mirror = sealed.seals[0].store();
                assert_ne!(mirror.identity(), store.identity());
                let mirror_bytes = mirror.file_bytes_for_test().unwrap();
                let retry =
                    crate::engine::scene::seal_native_sources(&mixed, &session, 1024, 16, &|| {
                        Ok(())
                    })
                    .unwrap();
                assert!(retry.seals[0].store().same_owner(&mirror));
                assert_eq!(mirror.file_bytes_for_test().unwrap(), mirror_bytes);
                for (_, surface) in sealed.rasters.iter() {
                    for tile in surface.physical_tiles() {
                        assert!(tile.owner().same_owner(&mirror));
                        let original = selected_refs
                            .iter()
                            .find(|reference| reference.digest() == tile.reference().digest())
                            .unwrap();
                        assert_eq!(original.spec(), tile.reference().spec());
                        assert_eq!(
                            store.pin(*original, &mut || Ok(())).unwrap().bytes(),
                            mirror
                                .pin(tile.reference(), &mut || Ok(()))
                                .unwrap()
                                .bytes(),
                        );
                    }
                }
                let output = crate::engine::native::OutputDraft::new(
                    mixed.rasters().ordinal(1).unwrap().clone(),
                    vec![],
                )
                .seal(engine.native_document().unwrap(), &session, &|| Ok(()))
                .unwrap_or_else(|failure| panic!("{}", failure.reason()));
                output.abort(&session, &|| Ok(())).unwrap();
                let capture = Capture::from_native(&mixed, &session, limits(), &|| Ok(())).unwrap();
                let snapshot = *uuid::Uuid::new_v4().as_bytes();
                capture.publish(snapshot, &session, &|| Ok(())).unwrap();
                let decoded = load(&session, snapshot, limits(), &|| Ok(())).unwrap();
                assert_eq!(decoded.seals.len(), 1);
                assert_eq!(
                    decoded.pending.rasters().len(),
                    if across_layers { 2 } else { 1 }
                );
                for ((_, before), (_, after)) in
                    mixed.rasters().iter().zip(decoded.pending.rasters().iter())
                {
                    let region = crate::raster::backed::Region::new([0, 0, 512, 256]).unwrap();
                    let before = before.prepare_region(region, &mut || Ok(())).unwrap();
                    let after = after.prepare_region(region, &mut || Ok(())).unwrap();
                    for (x, y) in [(2, 3), (258, 3), (511, 255)] {
                        assert_eq!(before.pixel16(x, y).unwrap(), after.pixel16(x, y).unwrap());
                    }
                }
                assert_eq!(mixed.key(), key);
                assert_eq!(engine.history_lengths(), [0, 0]);
                assert_eq!(store.checkpoint_reference().unwrap(), Some(selections[0]));
                assert_eq!(store.file_bytes_for_test().unwrap(), unchanged);
            }
            let native =
                crate::engine::native::NativeDocument::from_pending(metadata.unwrap()).unwrap();
            let engine = Engine::from_native(native, 8).unwrap();
            let scene = engine.capture_scene(None).unwrap().into_native().unwrap();
            let capture = Capture::from_native(&scene, &session, limits(), &|| Ok(())).unwrap();
            assert_eq!(scene.metadata().revision(), 1);
            let before = source_file_contents(&f);
            let next = *uuid::Uuid::new_v4().as_bytes();
            capture.publish(next, &session, &|| Ok(())).unwrap();
            let after = source_file_contents(&f);
            for (path, value) in &before {
                assert_eq!(after.get(path), Some(value));
            }
            assert_eq!(
                session
                    .checkpoint_census(&|| Ok(()))
                    .unwrap()
                    .registered_count(),
                6
            );
            let restored = crate::recovery::restore(
                &workspace,
                &f.base,
                &format!("native:{}", uuid::Uuid::from_bytes(next)),
                &Default::default(),
                None,
            )
            .unwrap();
            let scene = restored
                .lock()
                .unwrap()
                .capture_scene(None)
                .unwrap()
                .into_native()
                .unwrap();
            let raster = scene.rasters().iter().next().unwrap().1;
            let region = crate::raster::backed::Region::new([0, 0, 4, 4]).unwrap();
            assert_eq!(
                raster
                    .prepare_region(region, &mut || Ok(()))
                    .unwrap()
                    .pixel16(2, 3)
                    .unwrap(),
                expected[1]
            );
        }
    }
    #[test]
    fn recovery_registration_refuses_cancel_foreign_session_full_workspace_and_changed_marker() {
        for refusal in ["cancel", "session", "workspace", "full", "marker"] {
            let f = Fixture::new();
            let workspace = f.workspace();
            let engine = native_engine(&f, 16);
            let scene = engine.capture_scene(None).unwrap().into_native().unwrap();
            let session = f.runtime().begin().unwrap();
            let capture = Capture::from_native(&scene, &session, limits(), &|| Ok(())).unwrap();
            let id = *uuid::Uuid::new_v4().as_bytes();
            capture.publish(id, &session, &|| Ok(())).unwrap();
            if refusal == "full" {
                for _ in 1..crate::workspace::MAX_PROJECTS {
                    crate::workspace::new_project(f.shared.as_ref().unwrap(), 1, 1, 8, None)
                        .unwrap();
                }
            }
            let count = crate::workspace::entries_in(&workspace).len();
            let decoded = load(&session, id, limits(), &|| Ok(())).unwrap();
            let control = crate::loading::Control::default();
            let other = f.runtime().begin().unwrap();
            let foreign = Fixture::new();
            if refusal == "cancel" {
                control.cancel();
            }
            if refusal == "workspace" {
                workspace.lock().unwrap().source_runtime = Some(foreign.runtime().clone());
            }
            if refusal == "marker" {
                session.corrupt_checkpoint_for_test(id, 112).unwrap();
            }
            let bytes = marker_bytes(&session, id);
            let disk = session.disk().used().unwrap();
            let attempted = if refusal == "session" {
                &other
            } else {
                &session
            };
            assert!(
                crate::workspace::register_native_recovery(
                    &workspace, attempted, decoded, &control, None
                )
                .is_err(),
                "{refusal}"
            );
            assert_eq!(crate::workspace::entries_in(&workspace).len(), count);
            assert_eq!(marker_bytes(&session, id), bytes);
            assert_eq!(session.disk().used().unwrap(), disk);
        }
    }
    #[test]
    fn valid_marker_hash_never_admits_unsupported_native_metadata_as_editable_work() {
        for (from, to) in [
            (
                b"\"kind\":\"paint\"".as_slice(),
                b"\"kind\":\"taint\"".as_slice(),
            ),
            (
                b"\"blend\":\"normal\"".as_slice(),
                b"\"blend\":\"future\"".as_slice(),
            ),
            (
                b"\"kind\":\"blur\"".as_slice(),
                b"\"kind\":\"nope\"".as_slice(),
            ),
            (b"\"weight\":0.75".as_slice(), b"\"weight\":9.75".as_slice()),
        ] {
            let f = Fixture::new();
            let workspace = f.workspace();
            let engine = native_engine(&f, 16);
            let scene = engine.capture_scene(None).unwrap().into_native().unwrap();
            let session = f.runtime().begin().unwrap();
            let capture = Capture::from_native(&scene, &session, limits(), &|| Ok(())).unwrap();
            let id = *uuid::Uuid::new_v4().as_bytes();
            capture.publish(id, &session, &|| Ok(())).unwrap();
            let changed = rewritten(&session, marker_bytes(&session, id), |bytes| {
                let at = bytes.windows(from.len()).position(|b| b == from).unwrap();
                bytes[at..at + from.len()].copy_from_slice(to);
            });
            let decoded = load(&session, changed, limits(), &|| Ok(())).unwrap();
            let bytes = marker_bytes(&session, changed);
            let disk = session.disk().used().unwrap();
            assert!(crate::workspace::register_native_recovery(
                &workspace,
                &session,
                decoded,
                &Default::default(),
                None
            )
            .is_err());
            assert_eq!(crate::workspace::entries_in(&workspace).len(), 1);
            assert_eq!(marker_bytes(&session, changed), bytes);
            assert_eq!(session.disk().used().unwrap(), disk);
        }
    }
    #[test]
    fn missing_native_recovery_never_creates_a_source_namespace_or_tab() {
        let f = Fixture::new();
        let workspace = f.workspace();
        let source = f
            .connection
            .as_ref()
            .unwrap()
            .state_dir
            .join("native-sources-v1");
        assert!(!source.exists());
        assert!(crate::recovery::restore(
            &workspace,
            &f.base,
            &uuid::Uuid::new_v4().to_string(),
            &Default::default(),
            None
        )
        .is_err());
        assert!(!source.exists());
        assert_eq!(f.runtime().disk().used().unwrap(), 0);
        assert_eq!(crate::workspace::entries_in(&workspace).len(), 1);
    }
    #[test]
    fn catalog_lists_native_depths_and_damaged_records_without_changing_sources() {
        for depth in [8, 16] {
            let f = Fixture::new();
            let workspace = f.workspace();
            let engine = native_engine(&f, depth);
            let scene = engine.capture_scene(None).unwrap().into_native().unwrap();
            let session = f.runtime().begin().unwrap();
            let capture = Capture::from_native(&scene, &session, limits(), &|| Ok(())).unwrap();
            let id = *uuid::Uuid::new_v4().as_bytes();
            capture.publish(id, &session, &|| Ok(())).unwrap();
            let bytes = marker_bytes(&session, id);
            let damaged = rewritten(&session, bytes.clone(), |bytes| bytes[0] ^= 0x55);
            let damaged_bytes = marker_bytes(&session, damaged);
            let disk = session.disk().used().unwrap();
            let listed = crate::recovery::catalog_in(&workspace, &f.base, &|| Ok(())).unwrap();
            assert!(listed.native.error().is_none());
            assert_eq!(listed.native.entries().len(), 2);
            let good_id = format!("native:{}", uuid::Uuid::from_bytes(id));
            let good = listed
                .native
                .entries()
                .iter()
                .find(|e| e.snapshot == good_id)
                .unwrap();
            assert_eq!(good.bit_depth, Some(depth));
            assert_eq!(
                good.document_id.as_deref(),
                Some(scene.identity().document_id.as_str())
            );
            assert_eq!(good.display_name, scene.metadata().name());
            assert_eq!(good.read_only, Some(false));
            assert!(good.error.is_none());
            let bad_id = format!("native:{}", uuid::Uuid::from_bytes(damaged));
            let bad = listed
                .native
                .entries()
                .iter()
                .find(|e| e.snapshot == bad_id)
                .unwrap();
            assert!(bad.error.is_some());
            assert!(bad.document_id.is_none());
            assert!(bad.revision.is_none());
            assert!(bad.timestamp_ms.is_none());
            assert_eq!(marker_bytes(&session, id), bytes);
            assert_eq!(marker_bytes(&session, damaged), damaged_bytes);
            assert_eq!(session.disk().used().unwrap(), disk);
            // An active reader keeps discard busy, even for a valid checkpoint.
            assert!(crate::recovery::discard_in(&workspace, &f.base, &good_id).is_err());
            assert_eq!(marker_bytes(&session, id), bytes);
        }
    }
    #[test]
    fn mixed_catalog_identical_snapshot_uuids_restore_the_explicit_format() {
        let f = Fixture::new();
        let workspace = f.workspace();
        {
            let mut root = f.shared.as_ref().unwrap().lock().unwrap();
            root.resident_state_mut().unwrap().document = Document::new_depth(4, 2, 8).unwrap();
            let layer = root.resident_state().unwrap().document.layers[0].id.clone();
            root.edit(
                "human",
                &[serde_json::json!({"op":"paint.fill","layer":layer,"color":[17,29,43,255]})],
                None,
                None,
                "Legacy work",
            )
            .unwrap();
        }
        crate::recovery::checkpoint(&workspace, &f.base, &mut Default::default());
        let legacy = crate::recovery::catalog(&f.base).pop().unwrap();
        let legacy_bytes = fs::read(f.base.join(&legacy.file)).unwrap();
        let engine = native_engine(&f, 16);
        let scene = engine.capture_scene(None).unwrap().into_native().unwrap();
        let session = f.runtime().begin().unwrap();
        let id = *uuid::Uuid::parse_str(&legacy.snapshot).unwrap().as_bytes();
        Capture::from_native(&scene, &session, limits(), &|| Ok(()))
            .unwrap()
            .publish(id, &session, &|| Ok(()))
            .unwrap();
        let listed = crate::recovery::catalog_in(&workspace, &f.base, &|| Ok(())).unwrap();
        assert_eq!(listed.versions.len(), 1);
        assert_eq!(listed.native.entries().len(), 1);
        let native = crate::recovery::restore(
            &workspace,
            &f.base,
            &listed.native.entries()[0].snapshot,
            &Default::default(),
            None,
        )
        .unwrap();
        assert_eq!(
            native
                .lock()
                .unwrap()
                .native_document()
                .unwrap()
                .metadata()
                .depth(),
            16
        );
        let resident = crate::recovery::restore(
            &workspace,
            &f.base,
            &legacy.snapshot,
            &Default::default(),
            None,
        )
        .unwrap();
        assert_eq!(
            resident
                .lock()
                .unwrap()
                .resident_state()
                .unwrap()
                .document
                .bit_depth,
            8
        );
        assert_eq!(fs::read(f.base.join(&legacy.file)).unwrap(), legacy_bytes);
        assert!(session.open_recovery_marker(id).is_ok());
    }
    #[test]
    fn catalog_absence_cancel_and_admission_failure_never_drop_recovery_data() {
        let f = Fixture::new();
        let workspace = f.workspace();
        let source = f
            .connection
            .as_ref()
            .unwrap()
            .state_dir
            .join("native-sources-v1");
        let empty = crate::recovery::catalog_in(&workspace, &f.base, &|| Ok(())).unwrap();
        assert!(empty.native.entries().is_empty());
        assert!(empty.native.error().is_none());
        assert!(!source.exists());
        fs::create_dir(&source).unwrap();
        let incomplete = crate::recovery::catalog_in(&workspace, &f.base, &|| Ok(())).unwrap();
        assert!(incomplete.native.error().is_some());
        assert!(fs::read_dir(&source).unwrap().next().is_none());
        let engine = native_engine(&f, 16);
        let scene = engine.capture_scene(None).unwrap().into_native().unwrap();
        let session = f.runtime().begin().unwrap();
        let id = *uuid::Uuid::new_v4().as_bytes();
        Capture::from_native(&scene, &session, limits(), &|| Ok(()))
            .unwrap()
            .publish(id, &session, &|| Ok(()))
            .unwrap();
        let bytes = marker_bytes(&session, id);
        let disk = session.disk().used().unwrap();
        let calls = std::cell::Cell::new(0);
        assert!(crate::recovery::catalog_in(&workspace, &f.base, &|| {
            calls.set(calls.get() + 1);
            if calls.get() >= 12 {
                Err("Cancelled discovery".into())
            } else {
                Ok(())
            }
        })
        .is_err());
        let budget = f.runtime().resident();
        let remaining = budget.limit() - budget.used();
        let ballast = MetadataCharge::reserve(&budget, remaining, &|| Ok(())).unwrap();
        let refused = crate::recovery::catalog_in(&workspace, &f.base, &|| Ok(())).unwrap();
        assert!(refused.native.error().is_some());
        assert!(refused.native.entries().is_empty());
        drop(ballast);
        assert_eq!(marker_bytes(&session, id), bytes);
        assert_eq!(session.disk().used().unwrap(), disk);
        assert_eq!(crate::workspace::entries_in(&workspace).len(), 1);
    }
    #[test]
    #[cfg(windows)]
    fn discard_releases_only_one_marker_and_preserves_shared_native_sources_and_open_tabs() {
        for depth in [8, 16] {
            let f = Fixture::new();
            let workspace = f.workspace();
            let engine = native_engine(&f, depth);
            let key = engine.native_document().unwrap().key();
            let scene = engine.capture_scene(None).unwrap().into_native().unwrap();
            let first = uuid::Uuid::new_v4();
            let second = uuid::Uuid::new_v4();
            let marker_length;
            {
                let session = f.runtime().begin().unwrap();
                let capture = Capture::from_native(&scene, &session, limits(), &|| Ok(())).unwrap();
                for id in [first, second] {
                    capture
                        .publish(*id.as_bytes(), &session, &|| Ok(()))
                        .unwrap();
                }
                marker_length = session
                    .open_recovery_marker(*first.as_bytes())
                    .unwrap()
                    .len();
            }
            let restored = crate::recovery::restore(
                &workspace,
                &f.base,
                &format!("native:{first}"),
                &Default::default(),
                None,
            )
            .unwrap();
            let restored_key = restored.lock().unwrap().native_document().unwrap().key();
            let directory = f.base.join("state/native-sources-v1");
            let mut before = source_file_contents(&f);
            let marker = directory.join(format!("recovery-v3-{first}.pbr"));
            before.remove(&marker).unwrap();
            let disk = f.runtime().disk().used().unwrap();
            crate::recovery::discard_in(&workspace, &f.base, &format!("native:{first}")).unwrap();
            assert_eq!(source_file_contents(&f), before);
            assert_eq!(f.runtime().disk().used().unwrap(), disk - marker_length);
            assert_eq!(engine.native_document().unwrap().key(), key);
            assert_eq!(engine.history_lengths(), [0, 0]);
            let restored_scene = {
                let engine = restored.lock().unwrap();
                assert_eq!(engine.native_document().unwrap().key(), restored_key);
                assert_eq!(engine.history_lengths(), [0, 0]);
                engine.capture_scene(None).unwrap().into_native().unwrap()
            };
            for ((_, original), (_, restored)) in
                scene.rasters().iter().zip(restored_scene.rasters().iter())
            {
                let region = crate::raster::backed::Region::new([0, 0, 512, 256]).unwrap();
                let original = original.prepare_region(region, &mut || Ok(())).unwrap();
                let restored = restored.prepare_region(region, &mut || Ok(())).unwrap();
                for (x, y) in [(2, 3), (511, 255)] {
                    assert_eq!(
                        original.pixel16(x, y).unwrap(),
                        restored.pixel16(x, y).unwrap()
                    );
                }
            }
            let session = f.runtime().begin_existing().unwrap();
            assert!(session.open_recovery_marker(*first.as_bytes()).is_err());
            let remaining = load(&session, *second.as_bytes(), limits(), &|| Ok(())).unwrap();
            assert_eq!(remaining.pending.metadata().bit_depth, depth);
            assert_eq!(
                session
                    .checkpoint_census(&|| Ok(()))
                    .unwrap()
                    .registered_count(),
                1
            );
        }
    }
    fn source_file_contents(
        fixture: &Fixture,
    ) -> std::collections::BTreeMap<PathBuf, ((u64, u64), Vec<u8>)> {
        let session = fixture.runtime().begin_existing().unwrap();
        source_files(&session, &fixture.base.join("state/native-sources-v1"))
    }
    fn source_files(
        session: &SourceSession,
        directory: &std::path::Path,
    ) -> std::collections::BTreeMap<PathBuf, ((u64, u64), Vec<u8>)> {
        let mut files = std::collections::BTreeMap::new();
        for entry in fs::read_dir(directory).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                files.extend(source_files(session, &path));
            } else {
                let file = fs::File::open(&path).unwrap();
                files.insert(
                    path.clone(),
                    (
                        crate::file_safety::identity(&file).unwrap(),
                        session.owned_file_bytes_for_test(&path).unwrap(),
                    ),
                );
            }
        }
        files
    }
    #[test]
    #[cfg(windows)]
    fn discard_refuses_live_cursors_child_retention_cancellation_and_unknown_frames() {
        let f = Fixture::new();
        let engine = native_engine(&f, 16);
        let scene = engine.capture_scene(None).unwrap().into_native().unwrap();
        let session = f.runtime().begin().unwrap();
        let id = *uuid::Uuid::new_v4().as_bytes();
        Capture::from_native(&scene, &session, limits(), &|| Ok(()))
            .unwrap()
            .publish(id, &session, &|| Ok(()))
            .unwrap();
        let cursor = session.open_recovery_marker(id).unwrap();
        let disk = session.disk().used().unwrap();
        assert!(f
            .runtime()
            .begin_existing()
            .unwrap()
            .discard_checkpoint(id, limits(), &|| Ok(()))
            .is_err());
        drop(cursor);
        let bytes = marker_bytes(&session, id);
        let future = rewritten(&session, bytes.clone(), |bytes| bytes[19] = 4);
        drop(session);
        let session = f.runtime().begin_existing().unwrap();
        let child = session.retain_for_child().unwrap();
        assert!(session
            .discard_checkpoint(id, limits(), &|| Ok(()))
            .is_err());
        drop(child);
        assert!(f
            .runtime()
            .begin_existing()
            .unwrap()
            .discard_checkpoint(id, limits(), &|| Err("Cancelled".into()))
            .is_err());
        let charged = f.runtime().disk().used().unwrap();
        assert!(charged > disk);
        assert!(f
            .runtime()
            .begin_existing()
            .unwrap()
            .discard_checkpoint(future, limits(), &|| Ok(()))
            .is_err());
        let session = f.runtime().begin_existing().unwrap();
        assert_eq!(marker_bytes(&session, id), bytes);
        assert_eq!(f.runtime().disk().used().unwrap(), charged);
        drop(session);
        let marker = f.base.join(format!(
            "state/native-sources-v1/recovery-v3-{}.pbr",
            uuid::Uuid::from_bytes(id)
        ));
        let human_link = f.base.join("human-marker-link.pbr");
        fs::hard_link(&marker, &human_link).unwrap();
        assert!(f.runtime().begin_existing().is_err());
        assert_eq!(fs::read(&marker).unwrap(), bytes);
        assert_eq!(fs::read(&human_link).unwrap(), bytes);
        assert_eq!(f.runtime().disk().used().unwrap(), charged);
    }
    #[test]
    #[cfg(windows)]
    fn discard_restores_before_move_and_retains_roots_and_debt_after_uncertain_removal() {
        use crate::workspace::source_runtime::RetirementFault;
        for fault in [
            RetirementFault::Detached,
            RetirementFault::PanicDetached,
            RetirementFault::Moved,
            RetirementFault::Unlinked,
        ] {
            let f = Fixture::new();
            let engine = native_engine(&f, 16);
            let scene = engine.capture_scene(None).unwrap().into_native().unwrap();
            let session = f.runtime().begin().unwrap();
            let id = *uuid::Uuid::new_v4().as_bytes();
            Capture::from_native(&scene, &session, limits(), &|| Ok(()))
                .unwrap()
                .publish(id, &session, &|| Ok(()))
                .unwrap();
            let bytes = marker_bytes(&session, id);
            let disk = session.disk().used().unwrap();
            drop(session);
            let session = f.runtime().begin_existing().unwrap();
            let checks = std::cell::Cell::new(0);
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                session.discard_checkpoint_with_fault(
                    id,
                    limits(),
                    &|| {
                        let count = checks.get();
                        checks.set(count + 1);
                        if count != 0 {
                            assert!(f.runtime().begin_existing().is_err());
                            assert!(f.runtime().begin_existing_if_present().is_err());
                        }
                        Ok(())
                    },
                    fault,
                )
            }));
            if fault == RetirementFault::PanicDetached {
                assert!(result.is_err());
            } else {
                let error = result.unwrap().unwrap_err();
                assert!(
                    error.contains(if fault == RetirementFault::Detached {
                        "Cancelled after detach"
                    } else {
                        "disposal is uncertain"
                    }),
                    "{error}"
                );
            }
            assert_eq!(f.runtime().disk().used().unwrap(), disk);
            let session = f.runtime().begin_existing().unwrap();
            if matches!(
                fault,
                RetirementFault::Detached | RetirementFault::PanicDetached
            ) {
                assert_eq!(marker_bytes(&session, id), bytes);
                assert_eq!(
                    session
                        .checkpoint_census(&|| Ok(()))
                        .unwrap()
                        .registered_count(),
                    1
                );
            } else {
                assert!(session.recovery_marker_ids().is_err());
                assert!(session.checkpoint_census(&|| Ok(())).is_err());
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
    fn failed_durable_capture_latches_and_never_selects_failed_bytes() {
        let f = Fixture::new();
        let staged = stage(&f, 16);
        let session = f.runtime().begin().unwrap();
        let capture =
            Capture::new(&staged.scene, &staged.seals, &session, limits(), &|| Ok(())).unwrap();
        let id = *uuid::Uuid::new_v4().as_bytes();
        let pending = f.base.join(format!(
            "state/native-sources-v1/pending-{}",
            uuid::Uuid::from_bytes(id)
        ));
        let reached_write = std::cell::Cell::new(false);
        assert!(capture
            .publish(id, &session, &|| {
                // Admission/source proof may add checks before the writer is
                // created. Fail only after this actual writer has emitted bytes.
                if fs::symlink_metadata(&pending)
                    .is_ok_and(|m| m.is_file() && !m.file_type().is_symlink() && m.len() != 0)
                {
                    reached_write.set(true);
                    Err("Captured source changed during publication".into())
                } else {
                    Ok(())
                }
            })
            .is_err());
        assert!(
            reached_write.get(),
            "Failure must occur after actual pending bytes"
        );
        if cfg!(windows) {
            assert!(!pending.exists());
        } else {
            assert!(pending.is_file());
            assert!(fs::metadata(&pending).unwrap().len() != 0);
        }
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
    #[cfg(windows)]
    fn source_collection_keeps_registered_recovery_roots_until_the_marker_is_retired() {
        for depth in [8, 16] {
            let mut f = Fixture::new();
            let engine = native_engine(&f, depth);
            let scene = engine.capture_scene(None).unwrap().into_native().unwrap();
            let session = f.runtime().begin().unwrap();
            let id = *uuid::Uuid::new_v4().as_bytes();
            let capture = Capture::from_native(&scene, &session, limits(), &|| Ok(())).unwrap();
            capture.publish(id, &session, &|| Ok(())).unwrap();
            drop((capture, session, scene, engine));
            drop(f.runtime.take());
            f.runtime = Some(Fixture::new_runtime(f.connection.as_ref().unwrap()));
            let workspace = f.workspace();
            let before_unregistered = source_file_contents(&f);
            assert!(f
                .runtime()
                .begin_existing()
                .unwrap()
                .collect_sources(&|| Ok(()))
                .is_err());
            assert_eq!(source_file_contents(&f), before_unregistered);
            let catalog = crate::recovery::catalog_in(&workspace, &f.base, &|| Ok(())).unwrap();
            assert!(catalog.native.error().is_none());
            assert_eq!(catalog.native.entries().len(), 1);
            assert!(catalog.native.entries()[0].error.is_none());
            let before = source_file_contents(&f);
            let charged = f.runtime().disk().used().unwrap();
            let collected = f
                .runtime()
                .begin_existing()
                .unwrap()
                .collect_sources(&|| Ok(()))
                .unwrap();
            assert_eq!(collected.sources, 0);
            assert_eq!(source_file_contents(&f), before);
            assert_eq!(f.runtime().disk().used().unwrap(), charged);
            let session = f.runtime().begin_existing().unwrap();
            let decoded = load(&session, id, limits(), &|| Ok(())).unwrap();
            let raster = decoded.pending.rasters().ordinal(1).unwrap();
            let region = crate::raster::backed::Region::new([0, 0, 512, 256]).unwrap();
            assert!(
                raster
                    .prepare_region(region, &mut || Ok(()))
                    .unwrap()
                    .pixel16(2, 3)
                    .unwrap()[0]
                    > 0
            );
            drop(decoded);
            drop(session);
            f.runtime()
                .begin_existing()
                .unwrap()
                .discard_checkpoint(id, limits(), &|| Ok(()))
                .unwrap();
            let collected = f
                .runtime()
                .begin_existing()
                .unwrap()
                .collect_sources(&|| Ok(()))
                .unwrap();
            assert!(collected.sources > 0);
            assert!(collected.bytes > 0);
            assert_eq!(f.runtime().disk().used().unwrap(), 56);
            assert_eq!(
                f.runtime()
                    .begin_existing()
                    .unwrap()
                    .checkpoint_census(&|| Ok(()))
                    .unwrap()
                    .registered_count(),
                0
            );
        }
    }
    #[test]
    fn registered_checkpoint_keeps_selected_owners_after_live_scene_and_capture_drop() {
        let mut f = Fixture::new();
        let staged = stage(&f, 16);
        let session = f.runtime().begin().unwrap();
        let physical = staged.seals[0].store();
        let weak = physical.downgrade().unwrap();
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
    fn checkpoint_census_preserves_corrupt_or_unretired_marker_debt() {
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
            if corrupt || !cfg!(windows) {
                assert!(session.checkpoint_census(&|| Ok(())).is_err());
            } else {
                assert_eq!(
                    session
                        .checkpoint_census(&|| Ok(()))
                        .unwrap()
                        .registered_count(),
                    1
                );
            }
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
