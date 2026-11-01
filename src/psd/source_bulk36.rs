//! Portable native source roster derived from immutable physical references.
//! Metadata indexing does not pin/read an entire image or serialize Store paths.
use super::{
    bulk36::{self, BulkLimits, ChunkWriter, Descriptor, Digest, Record, Reference, Spec},
    manifest36::{self, Core, Raster, StagedCore},
};
use crate::{
    engine::scene::{Document36, SceneSnapshot},
    raster::surface::PhysicalTile,
};
use ring::digest::{Context, SHA256};
use std::{io::Write, ops::Range, sync::Arc};

struct Location {
    raster: u32,
    tile: PhysicalTile,
}
struct Chunk {
    records: Range<usize>,
    bytes: u32,
}
pub(crate) struct SourceIndex<'a> {
    scene: &'a SceneSnapshot,
    locations: Vec<Location>,
    unique: Vec<usize>,
    rasters: Vec<Raster>,
    chunks: Vec<Chunk>,
    sizes: Vec<u32>,
    tile_root: Digest,
    reference_root: Digest,
    _index: Arc<super::memory36::MetadataCharge>,
}
impl<'a> SourceIndex<'a> {
    pub(crate) fn new(
        scene: &'a SceneSnapshot,
        limits: BulkLimits,
        check: &dyn Fn() -> Result<(), String>,
    ) -> Result<Self, String> {
        check()?;
        if scene.rasters().len() > limits.rasters
            || limits.rasters > 65536
            || limits.references > 1_048_576
            || limits.tiles > 1_048_576
            || limits.chunks > 16384
        {
            return Err("Source36 physical source index exceeds declared admission".into());
        }
        let mut count = 0usize;
        for (_, source) in scene.rasters().iter() {
            check()?;
            if !source.uses_budget(scene.budget()) || !source.uses_disk_budget(scene.disk_budget())
            {
                return Err(
                    "Source36 physical source is outside the captured resource pools".into(),
                );
            }
            count = count
                .checked_add(source.physical_tiles().count())
                .ok_or("Source36 physical reference count overflow")?;
            if count > limits.references {
                return Err("Source36 physical references exceed declared admission".into());
            }
        }
        let max_chunks = (count
            .checked_mul(2)
            .ok_or("Source36 record count overflow")?)
        .min(limits.chunks);
        let bytes = count
            .checked_mul(std::mem::size_of::<Location>() + std::mem::size_of::<usize>())
            .and_then(|n| n.checked_add(scene.rasters().len() * std::mem::size_of::<Raster>()))
            .and_then(|n| {
                n.checked_add(
                    max_chunks * (std::mem::size_of::<Chunk>() + std::mem::size_of::<u32>()),
                )
            })
            .and_then(|n| n.checked_add(4096))
            .ok_or("Source36 physical metadata index overflow")?;
        let index = super::memory36::MetadataCharge::reserve(scene.budget(), bytes, check)?;
        let mut locations = Vec::new();
        locations
            .try_reserve_exact(count)
            .map_err(|_| "Source36 physical index allocation refused")?;
        let mut rasters = Vec::new();
        rasters
            .try_reserve_exact(scene.rasters().len())
            .map_err(|_| "Source36 raster index allocation refused")?;
        for (id, source) in scene.rasters().iter() {
            check()?;
            let start = locations.len();
            let spec = Spec::new(source.depth())?;
            for tile in source.physical_tiles() {
                check()?;
                let native = tile.reference().spec();
                let (x, y) = tile.coordinate();
                if native.width() != 256
                    || native.height() != 256
                    || native.depth() != spec.depth()
                    || x >= source.width().div_ceil(256)
                    || y >= source.height().div_ceil(256)
                    || !tile.owner().resident_budget().same_pool(scene.budget())
                    || !tile.owner().uses_disk_budget(scene.disk_budget())
                {
                    return Err("Source36 physical source tile shape/pools differ".into());
                }
                locations.push(Location { raster: id, tile });
            }
            rasters.push(Raster {
                id,
                width: source.width(),
                height: source.height(),
                depth: source.depth(),
                references: (locations.len() - start) as u32,
            });
        }
        if locations.len() != count {
            return Err(
                "Source36 captured physical roster changed during metadata indexing".into(),
            );
        }
        locations.sort_unstable_by_key(|l| {
            let (x, y) = l.tile.coordinate();
            (l.raster, y, x)
        });
        check()?;
        let mut unique = Vec::new();
        unique
            .try_reserve_exact(count)
            .map_err(|_| "Source36 unique tile index allocation refused")?;
        unique.extend(0..locations.len());
        unique.sort_unstable_by_key(|&i| {
            let r = locations[i].tile.reference();
            (r.spec().depth(), r.digest())
        });
        unique.dedup_by_key(|i| {
            let r = locations[*i].tile.reference();
            (r.spec().depth(), r.digest())
        });
        if unique.len() > limits.tiles {
            return Err("Source36 unique tiles exceed declared admission".into());
        }
        check()?;
        let mut tiles = root(b"PeerBrushSource36Tiles\0", unique.len());
        for &i in &unique {
            check()?;
            let reference = locations[i].tile.reference();
            let spec = Spec::new(reference.spec().depth())?;
            tiles.update(&spec.wire());
            tiles.update(&reference.digest());
            tiles.update(&(spec.bytes() as u32).to_be_bytes());
        }
        let mut refs = root(b"PeerBrushSource36Refs\0", locations.len());
        for location in &locations {
            check()?;
            let (x, y) = location.tile.coordinate();
            refs.update(&location.raster.to_be_bytes());
            refs.update(&x.to_be_bytes());
            refs.update(&y.to_be_bytes());
            refs.update(&location.tile.reference().digest());
        }
        let mut chunks: Vec<Chunk> = Vec::new();
        chunks
            .try_reserve_exact(max_chunks)
            .map_err(|_| "Source36 chunk plan allocation refused")?;
        let mut first = 0;
        let mut used = 56u32;
        let records = unique.len() + locations.len();
        let mut encoded = 0u64;
        for record in 0..records {
            check()?;
            let bytes = if record < unique.len() {
                48 + Spec::new(locations[unique[record]].tile.reference().spec().depth())?.bytes()
                    as u32
            } else {
                48
            };
            if used
                .checked_add(bytes)
                .is_none_or(|n| n > bulk36::MAX_CHUNK_BYTES)
            {
                push_chunk(&mut chunks, first..record, used, limits)?;
                encoded += u64::from(used);
                first = record;
                used = 56;
            }
            used = used
                .checked_add(bytes)
                .ok_or("Source36 chunk record size overflow")?;
        }
        if first != records {
            push_chunk(&mut chunks, first..records, used, limits)?;
            encoded += u64::from(used);
        }
        if encoded > limits.encoded_bytes {
            return Err("Source36 bulk payload exceeds declared disk admission".into());
        }
        let mut sizes = Vec::new();
        sizes
            .try_reserve_exact(chunks.len())
            .map_err(|_| "Source36 chunk size index allocation refused")?;
        sizes.extend(chunks.iter().map(|c| c.bytes));
        check()?;
        Ok(Self {
            scene,
            locations,
            unique,
            rasters,
            chunks,
            sizes,
            tile_root: finish(tiles),
            reference_root: finish(refs),
            _index: index,
        })
    }
    pub(crate) fn scene(&self) -> &SceneSnapshot {
        self.scene
    }
    pub(crate) fn sizes(&self) -> &[u32] {
        &self.sizes
    }
    pub(crate) fn rasters(&self) -> &[Raster] {
        &self.rasters
    }
    pub(crate) fn core(&self) -> Core<&'a Document36> {
        let doc = self.scene.metadata();
        Core {
            format: 36,
            source_id: doc.id.clone(),
            revision: doc.revision,
            metadata: doc,
            rasters: self.rasters.clone(),
            tiles: self.unique.len() as u32,
            references: self.locations.len() as u32,
            tile_root: manifest36::hex(self.tile_root),
            reference_root: manifest36::hex(self.reference_root),
        }
    }
    pub(crate) fn write(
        &self,
        ordinal: usize,
        core: Digest,
        output: &mut dyn Write,
        check: &dyn Fn() -> Result<(), String>,
    ) -> Result<Descriptor, String> {
        check()?;
        let chunk = self
            .chunks
            .get(ordinal)
            .ok_or("Missing Source36 chunk plan")?;
        let header = bulk36::Header {
            ordinal: ordinal as u32,
            total: self.chunks.len() as u32,
            records: chunk.records.len() as u32,
            core,
        };
        // Keep the trait object behind a sized reference for ChunkWriter's sink.
        let mut output = output;
        let mut writer = ChunkWriter::begin(&mut output, header, chunk.bytes, check)?;
        for record in chunk.records.clone() {
            check()?;
            if record < self.unique.len() {
                let location = &self.locations[self.unique[record]];
                let mut guarded = || {
                    check().map_err(|e| crate::large_document::Error::Io(std::io::Error::other(e)))
                };
                let pin = location
                    .tile
                    .owner()
                    .pin(location.tile.reference(), &mut guarded)
                    .map_err(|e| e.to_string())?;
                if !location
                    .tile
                    .owner()
                    .resident_budget()
                    .same_pool(self.scene.budget())
                {
                    return Err("Source36 tile pin uses a different resident pool".into());
                }
                let spec = Spec::new(location.tile.reference().spec().depth())?;
                writer.record(
                    Record::Tile {
                        spec,
                        digest: location.tile.reference().digest(),
                        payload: pin.bytes(),
                    },
                    check,
                )?;
            } else {
                let location = &self.locations[record - self.unique.len()];
                let (x, y) = location.tile.coordinate();
                writer.record(
                    Record::Reference(Reference {
                        raster: location.raster,
                        x,
                        y,
                        tile: location.tile.reference().digest(),
                    }),
                    check,
                )?;
            }
        }
        writer.finish(check)
    }
    pub(crate) fn check_core<T>(&self, core: &StagedCore<T>) -> Result<(), String> {
        if core.source_id() != self.scene.metadata().id
            || core.revision() != self.scene.metadata().revision
        {
            return Err("Source36 staged core belongs to another captured document".into());
        }
        Ok(())
    }
}
fn push_chunk(
    chunks: &mut Vec<Chunk>,
    records: Range<usize>,
    bytes: u32,
    limits: BulkLimits,
) -> Result<(), String> {
    if chunks.len() == limits.chunks || records.is_empty() {
        return Err("Source36 chunk plan exceeds declared admission".into());
    }
    chunks
        .try_reserve(1)
        .map_err(|_| "Source36 chunk plan allocation refused")?;
    chunks.push(Chunk { records, bytes });
    Ok(())
}
fn root(domain: &[u8], count: usize) -> Context {
    let mut hash = Context::new(&SHA256);
    hash.update(domain);
    hash.update(&(count as u64).to_be_bytes());
    hash
}
fn finish(hash: Context) -> Digest {
    hash.finish().as_ref().try_into().unwrap()
}
