//! Complete chunk-set/roster validation over caller-admitted index scratch.
//! No editable/source/recovery authority is granted by this component alone.
use super::{read_chunk, Context, Descriptor, Digest, Header, Order, Read, Record, Spec, SHA256};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct RasterSpec {
    pub id: u32,
    pub width: u32,
    pub height: u32,
    pub depth: u16,
    pub references: u32,
}
#[derive(Clone, Copy)]
pub(crate) struct BulkPlan<'a> {
    pub core: Digest,
    pub rasters: &'a [RasterSpec],
    pub tiles: u32,
    pub references: u32,
    pub tile_root: Digest,
    pub reference_root: Digest,
    pub chunks: &'a [Descriptor],
}
#[derive(Clone, Copy)]
pub(crate) struct BulkLimits {
    pub rasters: usize,
    pub tiles: usize,
    pub references: usize,
    pub chunks: usize,
    pub encoded_bytes: u64,
}
impl BulkPlan<'_> {
    /// Count/geometry admission before the caller allocates native/index scratch.
    pub(crate) fn admit_core(
        self,
        limits: BulkLimits,
        check: &dyn Fn() -> Result<(), String>,
    ) -> Result<(), String> {
        check()?;
        if limits.tiles > 1_048_576
            || limits.references > 1_048_576
            || limits.chunks > 16384
            || limits.rasters > 65536
            || self.tiles as usize > limits.tiles
            || self.references as usize > limits.references
            || self.rasters.len() > limits.rasters
        {
            return Err("Source36 bulk/index admission differs".into());
        }
        let mut references = 0u64;
        for (i, raster) in self.rasters.iter().enumerate() {
            check()?;
            if raster.id as usize != i + 1
                || raster.width == 0
                || raster.height == 0
                || raster.width > 16000
                || raster.height > 16000
            {
                return Err("Source36 raster roster is not canonical".into());
            }
            Spec::new(raster.depth)?;
            if u64::from(raster.references)
                > u64::from(raster.width.div_ceil(256)) * u64::from(raster.height.div_ceil(256))
            {
                return Err("Source36 raster reference count exceeds its geometry".into());
            }
            references = references
                .checked_add(u64::from(raster.references))
                .ok_or("Source36 reference count overflow")?;
        }
        if references != u64::from(self.references) {
            return Err("Source36 roster reference counts differ".into());
        }
        check()
    }
    pub(crate) fn admit(
        self,
        limits: BulkLimits,
        check: &dyn Fn() -> Result<(), String>,
    ) -> Result<(), String> {
        self.admit_core(limits, check)?;
        if self.chunks.len() > limits.chunks {
            return Err("Source36 chunk count exceeds admission".into());
        }
        let mut bytes = 0u64;
        for (i, chunk) in self.chunks.iter().enumerate() {
            check()?;
            if chunk.ordinal as usize != i
                || !(super::HEADER..=super::MAX_CHUNK_BYTES).contains(&chunk.bytes)
            {
                return Err("Source36 chunk roster is not canonical".into());
            }
            bytes = bytes
                .checked_add(u64::from(chunk.bytes))
                .ok_or("Source36 chunk byte count overflow")?;
        }
        if bytes > limits.encoded_bytes
            || ((self.tiles != 0 || self.references != 0) && self.chunks.is_empty())
        {
            return Err("Source36 encoded byte admission differs".into());
        }
        check()
    }
}

#[derive(Clone, Copy)]
pub(crate) struct TileIndexEntry {
    spec: Spec,
    digest: Digest,
    referenced: bool,
}
impl TileIndexEntry {
    pub(crate) fn vacant() -> Self {
        Self {
            spec: Spec::new(8).unwrap(),
            digest: [0; 32],
            referenced: false,
        }
    }
}

fn context(domain: &[u8], count: u32) -> Context {
    let mut hash = Context::new(&SHA256);
    hash.update(domain);
    hash.update(&u64::from(count).to_be_bytes());
    hash
}
fn fingerprint(
    plan: BulkPlan<'_>,
    check: &dyn Fn() -> Result<(), String>,
) -> Result<Digest, String> {
    let mut hash = Context::new(&SHA256);
    hash.update(b"PeerBrushSource36CheckedBulk\0");
    hash.update(&plan.core);
    hash.update(&plan.tiles.to_be_bytes());
    hash.update(&plan.references.to_be_bytes());
    hash.update(&(plan.rasters.len() as u64).to_be_bytes());
    for r in plan.rasters {
        check()?;
        for value in [r.id, r.width, r.height, u32::from(r.depth), r.references] {
            hash.update(&value.to_be_bytes());
        }
    }
    hash.update(&plan.tile_root);
    hash.update(&plan.reference_root);
    hash.update(&(plan.chunks.len() as u64).to_be_bytes());
    for c in plan.chunks {
        check()?;
        hash.update(&c.ordinal.to_be_bytes());
        hash.update(&c.bytes.to_be_bytes());
        hash.update(&c.sha256);
    }
    check()?;
    Ok(hash.finish().as_ref().try_into().unwrap())
}

pub(crate) struct BulkVerifier<'a, 'b> {
    plan: BulkPlan<'a>,
    entries: &'b mut [TileIndexEntry],
    raster_counts: &'b mut [u64],
    tiles: u32,
    references: u32,
    chunks: usize,
    order: Order,
    tile_root: Context,
    reference_root: Context,
    failed: bool,
}
impl<'a, 'b> BulkVerifier<'a, 'b> {
    /// Index/counter allocations are supplied and charged by the caller. This
    /// reports exact element storage only, excluding allocator/owner overhead.
    pub(crate) fn index_bytes(tiles: usize, rasters: usize) -> Result<usize, String> {
        tiles
            .checked_mul(std::mem::size_of::<TileIndexEntry>())
            .and_then(|n| {
                rasters
                    .checked_mul(std::mem::size_of::<u64>())
                    .and_then(|r| n.checked_add(r))
            })
            .ok_or_else(|| "Source36 metadata index size overflow".into())
    }
    pub(crate) fn new(
        plan: BulkPlan<'a>,
        limits: BulkLimits,
        entries: &'b mut [TileIndexEntry],
        raster_counts: &'b mut [u64],
        check: &dyn Fn() -> Result<(), String>,
    ) -> Result<Self, String> {
        plan.admit(limits, check)?;
        if entries.len() != plan.tiles as usize || raster_counts.len() != plan.rasters.len() {
            return Err("Source36 bulk/index admission differs".into());
        }
        for (i, entry) in entries.iter_mut().enumerate() {
            if i % 1024 == 0 {
                check()?;
            }
            *entry = TileIndexEntry::vacant();
        }
        raster_counts.fill(0);
        Ok(Self {
            plan,
            entries,
            raster_counts,
            tiles: 0,
            references: 0,
            chunks: 0,
            order: Order::default(),
            tile_root: context(b"PeerBrushSource36Tiles\0", plan.tiles),
            reference_root: context(b"PeerBrushSource36Refs\0", plan.references),
            failed: false,
        })
    }
    fn record(&mut self, record: &Record<'_>) -> Result<(), String> {
        match record {
            Record::Tile {
                spec,
                digest,
                payload,
            } => {
                if self.tiles == self.plan.tiles {
                    return Err("Excess Source36 native tiles".into());
                }
                self.order.tile(*spec, *digest)?;
                self.entries[self.tiles as usize] = TileIndexEntry {
                    spec: *spec,
                    digest: *digest,
                    referenced: false,
                };
                self.tile_root.update(&spec.wire());
                self.tile_root.update(digest);
                self.tile_root.update(&(payload.len() as u32).to_be_bytes());
                self.tiles += 1;
            }
            Record::Reference(r) => {
                if self.references == self.plan.references {
                    return Err("Excess Source36 raster references".into());
                }
                self.order.reference(*r)?;
                let index = r
                    .raster
                    .checked_sub(1)
                    .ok_or("Invalid Source36 raster ID")? as usize;
                let raster = self
                    .plan
                    .rasters
                    .get(index)
                    .ok_or("Missing Source36 raster descriptor")?;
                if r.x >= raster.width.div_ceil(256) || r.y >= raster.height.div_ceil(256) {
                    return Err("Source36 native tile lies outside its raster".into());
                }
                let spec = Spec::new(raster.depth)?;
                let at = self.entries[..self.tiles as usize]
                    .binary_search_by_key(&(spec, r.tile), |entry| (entry.spec, entry.digest))
                    .map_err(|_| "Missing or conflicting Source36 native tile")?;
                self.entries[at].referenced = true;
                self.raster_counts[index] += 1;
                if self.raster_counts[index] > u64::from(raster.references) {
                    return Err("Source36 raster reference count exceeds roster".into());
                }
                self.reference_root.update(&r.raster.to_be_bytes());
                self.reference_root.update(&r.x.to_be_bytes());
                self.reference_root.update(&r.y.to_be_bytes());
                self.reference_root.update(&r.tile);
                self.references += 1;
            }
        }
        Ok(())
    }
    /// Sink output stays provisional. A failed chunk poisons this entire bulk
    /// operation; it cannot resume with another chunk and later produce a token.
    pub(crate) fn chunk<R: Read>(
        &mut self,
        input: &mut R,
        header: Header,
        scratch: &mut [u8],
        check: &dyn Fn() -> Result<(), String>,
        mut sink: impl FnMut(Record<'_>) -> Result<(), String>,
    ) -> Result<(), String> {
        if self.failed {
            return Err("Source36 bulk verifier is failed".into());
        }
        let result = (|| {
            check()?;
            let descriptor = *self
                .plan
                .chunks
                .get(self.chunks)
                .ok_or("Excess Source36 chunks")?;
            if header.ordinal as usize != self.chunks
                || header.total as usize != self.plan.chunks.len()
                || header.core != self.plan.core
            {
                return Err("Source36 chunk set/order/core differs".into());
            }
            let token = read_chunk(input, header, descriptor, scratch, check, |record| {
                self.record(&record)?;
                sink(record)
            })?;
            token.check(header, descriptor)?;
            self.chunks += 1;
            check()
        })();
        self.failed |= result.is_err();
        result
    }
    pub(crate) fn finish(
        self,
        check: &dyn Fn() -> Result<(), String>,
    ) -> Result<CompletedBulk, String> {
        check()?;
        if self.failed
            || self.chunks != self.plan.chunks.len()
            || self.tiles != self.plan.tiles
            || self.references != self.plan.references
        {
            return Err("Source36 bulk set is incomplete or failed".into());
        }
        for entry in self.entries {
            check()?;
            if !entry.referenced {
                return Err("Orphan Source36 native tile".into());
            }
        }
        for (i, raster) in self.plan.rasters.iter().enumerate() {
            check()?;
            if self.raster_counts[i] != u64::from(raster.references) {
                return Err("Source36 raster coverage differs".into());
            }
        }
        if self.tile_root.finish().as_ref() != self.plan.tile_root
            || self.reference_root.finish().as_ref() != self.plan.reference_root
        {
            return Err("Source36 tile/reference roots differ".into());
        }
        Ok(CompletedBulk {
            fingerprint: fingerprint(self.plan, check)?,
        })
    }
}

/// Exact complete bulk plan, not a source lease, standard-composite proof or
/// editable-source grant. Typed metadata and all source/standard proofs remain.
#[must_use = "Bulk completion must be combined with standard and source publication checks"]
pub(crate) struct CompletedBulk {
    fingerprint: Digest,
}
impl CompletedBulk {
    pub(crate) fn check(
        &self,
        plan: BulkPlan<'_>,
        check: &dyn Fn() -> Result<(), String>,
    ) -> Result<(), String> {
        if fingerprint(plan, check)? != self.fingerprint {
            return Err("Source36 completion belongs to another bulk plan".into());
        }
        check()
    }
}

#[cfg(test)]
mod tests {
    use super::super::{tile_digest, ChunkWriter, Reference, MAX_TILE_BYTES};
    use super::*;
    use std::io::Cursor;

    struct Fixture {
        data: Vec<Vec<u8>>,
        headers: Vec<Header>,
        descriptors: Vec<Descriptor>,
        rasters: Vec<RasterSpec>,
        tile_root: Digest,
        reference_root: Digest,
    }
    fn root(domain: &[u8], records: &[Vec<u8>]) -> Digest {
        let mut bytes = domain.to_vec();
        bytes.extend_from_slice(&(records.len() as u64).to_be_bytes());
        for record in records {
            bytes.extend_from_slice(record);
        }
        ring::digest::digest(&SHA256, &bytes)
            .as_ref()
            .try_into()
            .unwrap()
    }
    fn fixture(variant: u8) -> Fixture {
        let check = || Ok(());
        let spec = Spec::new(16).unwrap();
        let mut tiles = [vec![17; spec.bytes()], vec![47; spec.bytes()]]
            .into_iter()
            .map(|payload| (tile_digest(spec, &payload, &check).unwrap(), payload))
            .collect::<Vec<_>>();
        tiles.sort_unstable_by_key(|(digest, _)| *digest);
        let mut references = [
            Reference {
                raster: 1,
                x: 62,
                y: 62,
                tile: tiles[0].0,
            },
            Reference {
                raster: 2,
                x: 0,
                y: 0,
                tile: tiles[1].0,
            },
        ];
        match variant {
            1 => references[1].tile = [0; 32],
            2 => references[1].tile = tiles[0].0,
            3 => references[0].x = 63,
            _ => {}
        }
        let tile_records = tiles
            .iter()
            .map(|(digest, payload)| {
                let mut record = spec.wire().to_vec();
                record.extend_from_slice(digest);
                record.extend_from_slice(&(payload.len() as u32).to_be_bytes());
                record
            })
            .collect::<Vec<_>>();
        let ref_records = references
            .iter()
            .map(|r| {
                let mut record = r.raster.to_be_bytes().to_vec();
                record.extend_from_slice(&r.x.to_be_bytes());
                record.extend_from_slice(&r.y.to_be_bytes());
                record.extend_from_slice(&r.tile);
                record
            })
            .collect::<Vec<_>>();
        let tile_root = root(b"PeerBrushSource36Tiles\0", &tile_records);
        let reference_root = root(b"PeerBrushSource36Refs\0", &ref_records);
        let mut data = vec![];
        let mut descriptors = vec![];
        let mut headers = vec![];
        for ordinal in 0..2 {
            let records = if variant == 4 {
                vec![
                    Record::Tile {
                        spec,
                        digest: tiles[ordinal].0,
                        payload: &tiles[ordinal].1,
                    },
                    Record::Reference(references[ordinal]),
                ]
            } else if ordinal == 0 {
                tiles
                    .iter()
                    .map(|(digest, payload)| Record::Tile {
                        spec,
                        digest: *digest,
                        payload,
                    })
                    .collect()
            } else {
                references.into_iter().map(Record::Reference).collect()
            };
            let length = super::super::HEADER
                + records
                    .iter()
                    .map(|record| match record {
                        Record::Tile { payload, .. } => 48 + payload.len() as u32,
                        Record::Reference(_) => 48,
                    })
                    .sum::<u32>();
            let header = Header {
                ordinal: ordinal as u32,
                total: 2,
                records: records.len() as u32,
                core: [37; 32],
            };
            let mut bytes = vec![];
            let mut writer = ChunkWriter::begin(&mut bytes, header, length, &check).unwrap();
            for record in records {
                writer.record(record, &check).unwrap();
            }
            descriptors.push(writer.finish(&check).unwrap());
            headers.push(header);
            data.push(bytes);
        }
        Fixture {
            data,
            headers,
            descriptors,
            rasters: vec![
                RasterSpec {
                    id: 1,
                    width: 16000,
                    height: 16000,
                    depth: 16,
                    references: 1,
                },
                RasterSpec {
                    id: 2,
                    width: 1,
                    height: 1,
                    depth: 16,
                    references: 1,
                },
            ],
            tile_root,
            reference_root,
        }
    }
    impl Fixture {
        fn plan(&self) -> BulkPlan<'_> {
            BulkPlan {
                core: [37; 32],
                rasters: &self.rasters,
                tiles: 2,
                references: 2,
                tile_root: self.tile_root,
                reference_root: self.reference_root,
                chunks: &self.descriptors,
            }
        }
        fn limits(&self) -> BulkLimits {
            BulkLimits {
                rasters: 2,
                tiles: 2,
                references: 2,
                chunks: 2,
                encoded_bytes: self.data.iter().map(|b| b.len() as u64).sum(),
            }
        }
    }

    #[test]
    fn complete_bulk_roster_hashes_and_token_bind_the_exact_plan() {
        let f = fixture(0);
        let check = || Ok(());
        let mut entries = [TileIndexEntry::vacant(); 2];
        let mut counters = [0; 2];
        assert_eq!(
            BulkVerifier::index_bytes(2, 2).unwrap(),
            std::mem::size_of_val(&entries) + std::mem::size_of_val(&counters)
        );
        let mut verifier =
            BulkVerifier::new(f.plan(), f.limits(), &mut entries, &mut counters, &check).unwrap();
        let mut scratch = vec![0; MAX_TILE_BYTES];
        let mut records = 0;
        for i in 0..2 {
            verifier
                .chunk(
                    &mut Cursor::new(&f.data[i]),
                    f.headers[i],
                    &mut scratch,
                    &check,
                    |_| {
                        records += 1;
                        Ok(())
                    },
                )
                .unwrap();
        }
        let token = verifier.finish(&check).unwrap();
        token.check(f.plan(), &check).unwrap();
        assert!(token
            .check(
                BulkPlan {
                    core: [99; 32],
                    ..f.plan()
                },
                &check
            )
            .is_err());
        assert_eq!(records, 4);
        assert_eq!(counters, [1, 1]);
    }

    #[test]
    fn missing_orphan_outside_raster_and_cross_chunk_order_cannot_complete() {
        for variant in 1..=4 {
            let f = fixture(variant);
            let check = || Ok(());
            let mut entries = [TileIndexEntry::vacant(); 2];
            let mut counts = [0; 2];
            let mut verifier =
                BulkVerifier::new(f.plan(), f.limits(), &mut entries, &mut counts, &check).unwrap();
            let mut scratch = vec![0; MAX_TILE_BYTES];
            let mut failed = false;
            for i in 0..2 {
                if verifier
                    .chunk(
                        &mut Cursor::new(&f.data[i]),
                        f.headers[i],
                        &mut scratch,
                        &check,
                        |_| Ok(()),
                    )
                    .is_err()
                {
                    failed = true;
                    break;
                }
            }
            let complete = verifier.finish(&check).is_ok();
            assert!(!complete, "variant={variant}, failed_chunk={failed}");
        }
    }

    #[test]
    fn lost_corrupt_retried_or_reordered_chunks_cannot_regain_completion() {
        let mut f = fixture(0);
        let check = || Ok(());
        f.descriptors[0].sha256[0] ^= 1;
        let mut entries = [TileIndexEntry::vacant(); 2];
        let mut counts = [0; 2];
        let mut verifier =
            BulkVerifier::new(f.plan(), f.limits(), &mut entries, &mut counts, &check).unwrap();
        let mut scratch = vec![0; MAX_TILE_BYTES];
        assert!(verifier
            .chunk(
                &mut Cursor::new(&f.data[0]),
                f.headers[0],
                &mut scratch,
                &check,
                |_| Ok(())
            )
            .is_err());
        assert!(verifier
            .chunk(
                &mut Cursor::new(&f.data[1]),
                f.headers[1],
                &mut scratch,
                &check,
                |_| Ok(())
            )
            .is_err());
        assert!(verifier.finish(&check).is_err());
        let f = fixture(0);
        let mut verifier =
            BulkVerifier::new(f.plan(), f.limits(), &mut entries, &mut counts, &check).unwrap();
        verifier
            .chunk(
                &mut Cursor::new(&f.data[0]),
                f.headers[0],
                &mut scratch,
                &check,
                |_| Ok(()),
            )
            .unwrap();
        assert!(
            verifier.finish(&check).is_err(),
            "One missing chunk cannot complete"
        );
        let mut verifier =
            BulkVerifier::new(f.plan(), f.limits(), &mut entries, &mut counts, &check).unwrap();
        assert!(verifier
            .chunk(
                &mut Cursor::new(&f.data[1]),
                f.headers[1],
                &mut scratch,
                &check,
                |_| Ok(())
            )
            .is_err());
        assert!(verifier.finish(&check).is_err());
    }

    #[test]
    fn constructor_admits_counts_before_indexes_and_exact_roots_remain_required() {
        let f = fixture(0);
        let check = || Ok(());
        let mut entries = [TileIndexEntry::vacant(); 2];
        let mut counts = [0; 2];
        let mut limits = f.limits();
        limits.encoded_bytes -= 1;
        assert!(BulkVerifier::new(f.plan(), limits, &mut entries, &mut counts, &check).is_err());
        let mut root = f.tile_root;
        root[0] ^= 1;
        let mut verifier = BulkVerifier::new(
            BulkPlan {
                tile_root: root,
                ..f.plan()
            },
            f.limits(),
            &mut entries,
            &mut counts,
            &check,
        )
        .unwrap();
        let mut scratch = vec![0; MAX_TILE_BYTES];
        for i in 0..2 {
            verifier
                .chunk(
                    &mut Cursor::new(&f.data[i]),
                    f.headers[i],
                    &mut scratch,
                    &check,
                    |_| Ok(()),
                )
                .unwrap();
        }
        assert!(verifier.finish(&check).is_err());
    }
}
