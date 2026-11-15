//! Source36 chunk framing only. Blocks emitted by a sink remain provisional.
//! Semantic DTO, roster/root, original-standard and source-publication proofs
//! are separate requirements; a completed chunk never grants editable authority.
use ring::digest::{Context, SHA256};
use std::io::{Read, Write};
#[path = "bulk36_verify.rs"]
mod verify;
pub(crate) use verify::{
    BulkLimits, BulkPlan, BulkVerifier, CompletedBulk, RasterSpec, TileIndexEntry,
};

pub(crate) type Digest = [u8; 32];
const MAGIC: &[u8; 8] = b"PBRBULK1";
pub(crate) const KEY: &[u8; 4] = b"Pb36";
pub(crate) const MAX_CHUNK_BYTES: u32 = 64 * 1024 * 1024;
pub(crate) const MAX_TILE_BYTES: usize = 256 * 256 * 4 * 2;
const HEADER: u32 = 56;
const TRANSFER: usize = 65536;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub(crate) struct Spec {
    depth: u16,
}
impl Spec {
    pub(crate) fn new(depth: u16) -> Result<Self, String> {
        if ![8, 16].contains(&depth) {
            return Err("Source36 requires native8/16 tiles".into());
        }
        Ok(Self { depth })
    }
    pub(crate) fn depth(self) -> u16 {
        self.depth
    }
    pub(crate) fn bytes(self) -> usize {
        256 * 256 * 4 * usize::from(self.depth / 8)
    }
    pub(super) fn wire(self) -> [u8; 8] {
        let mut out = [1, 0, 1, 0, 0, 0, 0, 4];
        out[4..6].copy_from_slice(&self.depth.to_be_bytes());
        out
    }
    fn parse(bytes: [u8; 8]) -> Result<Self, String> {
        let spec = Self::new(u16::from_be_bytes(bytes[4..6].try_into().unwrap()))?;
        if bytes != spec.wire() {
            return Err("Source36 tile spec is not full256 RGBA".into());
        }
        Ok(spec)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct Header {
    pub ordinal: u32,
    pub total: u32,
    pub records: u32,
    pub core: Digest,
}
impl Header {
    fn validate(self) -> Result<(), String> {
        if self.total == 0
            || self.ordinal >= self.total
            || self.records == 0
            || self.records > (MAX_CHUNK_BYTES - HEADER) / 48
        {
            return Err("Source36 chunk header exceeds admission".into());
        }
        Ok(())
    }
    fn wire(self) -> Result<[u8; HEADER as usize], String> {
        self.validate()?;
        let mut bytes = [0; HEADER as usize];
        bytes[..8].copy_from_slice(MAGIC);
        bytes[8..12].copy_from_slice(&36u32.to_be_bytes());
        bytes[12..16].copy_from_slice(&self.ordinal.to_be_bytes());
        bytes[16..20].copy_from_slice(&self.total.to_be_bytes());
        bytes[20..24].copy_from_slice(&self.records.to_be_bytes());
        bytes[24..].copy_from_slice(&self.core);
        Ok(bytes)
    }
    fn parse(bytes: &[u8; HEADER as usize]) -> Result<Self, String> {
        if &bytes[..8] != MAGIC || bytes[8..12] != 36u32.to_be_bytes() {
            return Err("Invalid Source36 chunk magic/version".into());
        }
        let result = Self {
            ordinal: u32_at(bytes, 12),
            total: u32_at(bytes, 16),
            records: u32_at(bytes, 20),
            core: bytes[24..].try_into().unwrap(),
        };
        result.validate()?;
        Ok(result)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct Descriptor {
    pub ordinal: u32,
    pub bytes: u32,
    pub sha256: Digest,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct Reference {
    pub raster: u32,
    pub x: u32,
    pub y: u32,
    pub tile: Digest,
}
pub(crate) enum Record<'a> {
    Tile {
        spec: Spec,
        digest: Digest,
        payload: &'a [u8],
    },
    Reference(Reference),
}

/// Local record order. Cross-chunk order, raster bounds, tile/reference roots,
/// missing/orphan detection and exact roster coverage remain caller obligations.
#[derive(Default)]
struct Order {
    tile: Option<(Spec, Digest)>,
    reference: Option<(u32, u32, u32)>,
}
impl Order {
    fn tile(&mut self, spec: Spec, digest: Digest) -> Result<(), String> {
        if self.reference.is_some() || self.tile.is_some_and(|old| old >= (spec, digest)) {
            return Err("Source36 tile order or uniqueness differs".into());
        }
        self.tile = Some((spec, digest));
        Ok(())
    }
    fn reference(&mut self, r: Reference) -> Result<(), String> {
        let key = (r.raster, r.y, r.x);
        if r.raster == 0 || self.reference.is_some_and(|old| old >= key) {
            return Err("Source36 reference order or uniqueness differs".into());
        }
        self.reference = Some(key);
        Ok(())
    }
}

pub(crate) fn tile_digest(
    spec: Spec,
    payload: &[u8],
    check: &dyn Fn() -> Result<(), String>,
) -> Result<Digest, String> {
    if payload.len() != spec.bytes() {
        return Err("Source36 tile payload length differs".into());
    }
    let mut hash = Context::new(&SHA256);
    hash.update(b"TILE");
    hash.update(&spec.wire());
    for part in payload.chunks(TRANSFER) {
        check()?;
        hash.update(part);
    }
    check()?;
    Ok(hash.finish().as_ref().try_into().unwrap())
}

pub(crate) struct ChunkWriter<'a, W> {
    output: &'a mut W,
    header: Header,
    expected: u32,
    written: u32,
    records: u32,
    hash: Context,
    order: Order,
    failed: bool,
}
impl<'a, W: Write> ChunkWriter<'a, W> {
    pub(crate) fn begin(
        output: &'a mut W,
        header: Header,
        bytes: u32,
        check: &dyn Fn() -> Result<(), String>,
    ) -> Result<Self, String> {
        if !(HEADER..=MAX_CHUNK_BYTES).contains(&bytes) {
            return Err("Source36 chunk byte admission differs".into());
        }
        let wire = header.wire()?;
        let mut writer = Self {
            output,
            header,
            expected: bytes,
            written: 0,
            records: 0,
            hash: Context::new(&SHA256),
            order: Order::default(),
            failed: false,
        };
        writer.raw(&wire, check)?;
        Ok(writer)
    }
    fn raw(&mut self, bytes: &[u8], check: &dyn Fn() -> Result<(), String>) -> Result<(), String> {
        if self.failed
            || self
                .written
                .checked_add(bytes.len() as u32)
                .is_none_or(|n| n > self.expected)
        {
            return Err("Source36 chunk is failed or exceeds its declared length".into());
        }
        for part in bytes.chunks(TRANSFER) {
            check()?;
            self.output.write_all(part).map_err(|e| e.to_string())?;
            self.hash.update(part);
            self.written += part.len() as u32;
        }
        check()
    }
    pub(crate) fn record(
        &mut self,
        record: Record<'_>,
        check: &dyn Fn() -> Result<(), String>,
    ) -> Result<(), String> {
        if self.failed {
            return Err("Source36 chunk writer is failed".into());
        }
        let result = self.record_inner(record, check);
        self.failed |= result.is_err();
        result
    }
    fn record_inner(
        &mut self,
        record: Record<'_>,
        check: &dyn Fn() -> Result<(), String>,
    ) -> Result<(), String> {
        check()?;
        if self.records == self.header.records {
            return Err("Excess Source36 chunk records".into());
        }
        match record {
            Record::Tile {
                spec,
                digest,
                payload,
            } => {
                if tile_digest(spec, payload, check)? != digest {
                    return Err("Source36 tile hash differs".into());
                }
                self.order.tile(spec, digest)?;
                self.raw(&1u32.to_be_bytes(), check)?;
                self.raw(&spec.wire(), check)?;
                self.raw(&digest, check)?;
                self.raw(&(payload.len() as u32).to_be_bytes(), check)?;
                self.raw(payload, check)?;
            }
            Record::Reference(r) => {
                self.order.reference(r)?;
                self.raw(&2u32.to_be_bytes(), check)?;
                self.raw(&r.raster.to_be_bytes(), check)?;
                self.raw(&r.x.to_be_bytes(), check)?;
                self.raw(&r.y.to_be_bytes(), check)?;
                self.raw(&r.tile, check)?;
            }
        }
        self.records += 1;
        check()
    }
    pub(crate) fn finish(
        self,
        check: &dyn Fn() -> Result<(), String>,
    ) -> Result<Descriptor, String> {
        check()?;
        if self.failed || self.records != self.header.records || self.written != self.expected {
            return Err("Source36 chunk did not complete exactly".into());
        }
        self.output.flush().map_err(|e| e.to_string())?;
        check()?;
        Ok(Descriptor {
            ordinal: self.header.ordinal,
            bytes: self.written,
            sha256: self.hash.finish().as_ref().try_into().unwrap(),
        })
    }
}

struct Input<'a, R> {
    input: &'a mut R,
    bytes: u32,
    expected: u32,
    hash: Context,
}
impl<R: Read> Input<'_, R> {
    fn exact(
        &mut self,
        out: &mut [u8],
        check: &dyn Fn() -> Result<(), String>,
    ) -> Result<(), String> {
        if self
            .bytes
            .checked_add(out.len() as u32)
            .is_none_or(|n| n > self.expected)
        {
            return Err("Source36 record exceeds chunk range".into());
        }
        for part in out.chunks_mut(TRANSFER) {
            check()?;
            self.input.read_exact(part).map_err(|e| e.to_string())?;
            self.hash.update(part);
            self.bytes += part.len() as u32;
        }
        check()
    }
    fn array<const N: usize>(
        &mut self,
        check: &dyn Fn() -> Result<(), String>,
    ) -> Result<[u8; N], String> {
        let mut bytes = [0; N];
        self.exact(&mut bytes, check)?;
        Ok(bytes)
    }
    fn u32(&mut self, check: &dyn Fn() -> Result<(), String>) -> Result<u32, String> {
        Ok(u32::from_be_bytes(self.array(check)?))
    }
}

#[must_use = "Chunk completion is provisional until the whole source and all required roots validate"]
pub(crate) struct CompletedChunk {
    header: Header,
    descriptor: Descriptor,
}
impl CompletedChunk {
    pub(crate) fn check(&self, header: Header, descriptor: Descriptor) -> Result<(), String> {
        if self.header != header || self.descriptor != descriptor {
            return Err("Source36 completion belongs to another chunk".into());
        }
        Ok(())
    }
}

/// `input` must be the bounded original tag-payload range. `scratch` must be a
/// caller-admitted mutable native tile buffer; this reader allocates no payload.
/// Sink output remains provisional even if all individual tile digests match.
pub(crate) fn read_chunk<R: Read>(
    input: &mut R,
    header: Header,
    descriptor: Descriptor,
    scratch: &mut [u8],
    check: &dyn Fn() -> Result<(), String>,
    mut sink: impl FnMut(Record<'_>) -> Result<(), String>,
) -> Result<CompletedChunk, String> {
    check()?;
    header.validate()?;
    if descriptor.ordinal != header.ordinal
        || !(HEADER..=MAX_CHUNK_BYTES).contains(&descriptor.bytes)
    {
        return Err("Source36 chunk descriptor differs".into());
    }
    let mut input = Input {
        input,
        bytes: 0,
        expected: descriptor.bytes,
        hash: Context::new(&SHA256),
    };
    if Header::parse(&input.array(check)?)? != header {
        return Err("Source36 chunk core/order binding differs".into());
    }
    let mut order = Order::default();
    for _ in 0..header.records {
        match input.u32(check)? {
            1 => {
                let spec = Spec::parse(input.array(check)?)?;
                let digest = input.array(check)?;
                let bytes = input.u32(check)? as usize;
                if bytes != spec.bytes() || bytes > scratch.len() {
                    return Err("Source36 tile scratch/length admission differs".into());
                }
                input.exact(&mut scratch[..bytes], check)?;
                if tile_digest(spec, &scratch[..bytes], check)? != digest {
                    return Err("Source36 native tile hash differs".into());
                }
                order.tile(spec, digest)?;
                sink(Record::Tile {
                    spec,
                    digest,
                    payload: &scratch[..bytes],
                })?;
            }
            2 => {
                let reference = Reference {
                    raster: input.u32(check)?,
                    x: input.u32(check)?,
                    y: input.u32(check)?,
                    tile: input.array(check)?,
                };
                order.reference(reference)?;
                sink(Record::Reference(reference))?;
            }
            _ => return Err("Unknown Source36 record kind".into()),
        }
        check()?;
    }
    let mut extra = [0; 1];
    if input.bytes != input.expected
        || input.input.read(&mut extra).map_err(|e| e.to_string())? != 0
        || input.hash.finish().as_ref() != descriptor.sha256
    {
        return Err("Source36 chunk has trailing, truncated or changed bytes".into());
    }
    check()?;
    Ok(CompletedChunk { header, descriptor })
}

fn u32_at(bytes: &[u8], at: usize) -> u32 {
    u32::from_be_bytes(bytes[at..at + 4].try_into().unwrap())
}

pub(super) fn standard_hasher(layer_bytes: u64, composite_bytes: u64) -> Context {
    let mut hash = Context::new(&SHA256);
    hash.update(b"PeerBrushSource36Standard\0");
    hash.update(&layer_bytes.to_be_bytes());
    hash.update(&composite_bytes.to_be_bytes());
    hash
}

/// Exact length-delimited original standard bytes; FNV is deliberately absent.
#[cfg(test)]
pub(crate) fn standard_digest<L: Read, C: Read>(
    layer: &mut L,
    layer_bytes: u64,
    composite: &mut C,
    composite_bytes: u64,
    check: &dyn Fn() -> Result<(), String>,
) -> Result<Digest, String> {
    let mut hash = standard_hasher(layer_bytes, composite_bytes);
    let mut buffer = [0; TRANSFER];
    for (input, length) in [
        (layer as &mut dyn Read, layer_bytes),
        (composite as &mut dyn Read, composite_bytes),
    ] {
        let mut remaining = length;
        while remaining != 0 {
            check()?;
            let bytes = remaining.min(TRANSFER as u64) as usize;
            input
                .read_exact(&mut buffer[..bytes])
                .map_err(|e| e.to_string())?;
            hash.update(&buffer[..bytes]);
            remaining -= bytes as u64;
        }
        check()?;
        if input.read(&mut buffer[..1]).map_err(|e| e.to_string())? != 0 {
            return Err("Source36 standard range length differs".into());
        }
    }
    check()?;
    Ok(hash.finish().as_ref().try_into().unwrap())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    use std::io::Cursor;

    fn payload(spec: Spec) -> Vec<u8> {
        (0..spec.bytes())
            .map(|n| ((n * 173 + n / 257 * 71) % 256) as u8)
            .collect()
    }
    fn fixture(depth: u16) -> (Vec<u8>, Header, Descriptor, Vec<u8>) {
        let spec = Spec::new(depth).unwrap();
        let data = payload(spec);
        let check = || Ok(());
        let digest = tile_digest(spec, &data, &check).unwrap();
        let header = Header {
            ordinal: 0,
            total: 1,
            records: 2,
            core: [37; 32],
        };
        let mut bytes = vec![];
        let mut writer =
            ChunkWriter::begin(&mut bytes, header, HEADER + 96 + data.len() as u32, &check)
                .unwrap();
        writer
            .record(
                Record::Tile {
                    spec,
                    digest,
                    payload: &data,
                },
                &check,
            )
            .unwrap();
        writer
            .record(
                Record::Reference(Reference {
                    raster: 1,
                    x: 62,
                    y: 62,
                    tile: digest,
                }),
                &check,
            )
            .unwrap();
        let descriptor = writer.finish(&check).unwrap();
        (bytes, header, descriptor, data)
    }
    fn rehash(bytes: &[u8], descriptor: Descriptor) -> Descriptor {
        Descriptor {
            bytes: bytes.len() as u32,
            sha256: ring::digest::digest(&SHA256, bytes)
                .as_ref()
                .try_into()
                .unwrap(),
            ..descriptor
        }
    }

    #[test]
    fn native_tiles_roundtrip_without_projection_or_hidden_payload_allocation() {
        for depth in [8, 16] {
            let (bytes, header, descriptor, data) = fixture(depth);
            let mut scratch = vec![0; MAX_TILE_BYTES];
            let mut records = 0;
            let token = read_chunk(
                &mut Cursor::new(&bytes),
                header,
                descriptor,
                &mut scratch,
                &|| Ok(()),
                |record| {
                    match record {
                        Record::Tile {
                            spec,
                            payload,
                            digest,
                        } => {
                            assert_eq!(spec.depth(), depth);
                            assert_eq!(payload, data);
                            let mut independent = Context::new(&SHA256);
                            independent.update(b"TILE");
                            independent.update(&[1, 0, 1, 0, 0, depth as u8, 0, 4]);
                            independent.update(payload);
                            assert_eq!(independent.finish().as_ref(), digest);
                            assert_eq!(records, 0);
                        }
                        Record::Reference(r) => {
                            assert_eq!((r.raster, r.x, r.y), (1, 62, 62));
                            assert_eq!(records, 1);
                        }
                    }
                    records += 1;
                    Ok(())
                },
            )
            .unwrap();
            token.check(header, descriptor).unwrap();
            assert!(token
                .check(
                    Header {
                        ordinal: 1,
                        total: 2,
                        ..header
                    },
                    descriptor
                )
                .is_err());
            assert_eq!(records, 2);
        }
    }

    #[test]
    fn malformed_headers_specs_hashes_kinds_lengths_and_trailing_records_never_complete() {
        let (bytes, header, descriptor, _) = fixture(16);
        let mut variants = vec![];
        for offset in [0, 8, 12, 16, 20, 24, 56, 60, 68, 100, 104] {
            let mut changed = bytes.clone();
            changed[offset] ^= 0x80;
            variants.push(changed);
        }
        variants.push(bytes[..bytes.len() - 1].to_vec());
        variants.push([bytes.as_slice(), &[0]].concat());
        for changed in variants {
            // Recompute the outer hash as well: malformed structure/native tile
            // data cannot become valid merely by updating its chunk checksum.
            let admitted = rehash(&changed, descriptor);
            assert!(read_chunk(
                &mut Cursor::new(&changed),
                header,
                admitted,
                &mut vec![0; MAX_TILE_BYTES],
                &|| Ok(()),
                |_| Ok(())
            )
            .is_err());
        }
        let mut corrupted_descriptor = descriptor;
        corrupted_descriptor.sha256[0] ^= 1;
        let seen = Cell::new(0);
        assert!(read_chunk(
            &mut Cursor::new(&bytes),
            header,
            corrupted_descriptor,
            &mut vec![0; MAX_TILE_BYTES],
            &|| Ok(()),
            |_| {
                seen.set(seen.get() + 1);
                Ok(())
            }
        )
        .is_err());
        assert_eq!(
            seen.get(),
            2,
            "Individually validated sink output is still provisional"
        );
        assert!(read_chunk(
            &mut Cursor::new(&bytes),
            header,
            descriptor,
            &mut vec![0; 1],
            &|| Ok(()),
            |_| Ok(())
        )
        .is_err());
    }

    #[test]
    fn duplicate_out_of_order_or_failed_writer_records_cannot_later_finish() {
        let spec = Spec::new(8).unwrap();
        let data = payload(spec);
        let check = || Ok(());
        let digest = tile_digest(spec, &data, &check).unwrap();
        let header = Header {
            ordinal: 0,
            total: 1,
            records: 2,
            core: [1; 32],
        };
        let mut bytes = vec![];
        let mut writer = ChunkWriter::begin(
            &mut bytes,
            header,
            HEADER + 96 + 2 * data.len() as u32,
            &check,
        )
        .unwrap();
        writer
            .record(
                Record::Tile {
                    spec,
                    digest,
                    payload: &data,
                },
                &check,
            )
            .unwrap();
        assert!(writer
            .record(
                Record::Tile {
                    spec,
                    digest,
                    payload: &data
                },
                &check
            )
            .is_err());
        assert!(writer.finish(&check).is_err());
        let mut bytes = vec![];
        let mut writer =
            ChunkWriter::begin(&mut bytes, header, HEADER + 96 + data.len() as u32, &check)
                .unwrap();
        writer
            .record(
                Record::Reference(Reference {
                    raster: 1,
                    x: 0,
                    y: 0,
                    tile: digest,
                }),
                &check,
            )
            .unwrap();
        assert!(writer
            .record(
                Record::Tile {
                    spec,
                    digest,
                    payload: &data
                },
                &check
            )
            .is_err());
        assert!(writer.finish(&check).is_err());
    }

    #[test]
    fn transfer_reads_stay_bounded_and_cancellation_and_sink_errors_propagate() {
        struct Observed<'a> {
            input: Cursor<&'a [u8]>,
            maximum: &'a Cell<usize>,
        }
        impl Read for Observed<'_> {
            fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
                self.maximum.set(self.maximum.get().max(out.len()));
                self.input.read(out)
            }
        }
        let (bytes, header, descriptor, _) = fixture(16);
        let maximum = Cell::new(0);
        let mut input = Observed {
            input: Cursor::new(&bytes),
            maximum: &maximum,
        };
        read_chunk(
            &mut input,
            header,
            descriptor,
            &mut vec![0; MAX_TILE_BYTES],
            &|| Ok(()),
            |_| Ok(()),
        )
        .unwrap()
        .check(header, descriptor)
        .unwrap();
        assert!(maximum.get() <= TRANSFER);
        let canceled = Cell::new(false);
        let check = || {
            if canceled.get() {
                Err("Canceled Source36 fixture".into())
            } else {
                Ok(())
            }
        };
        let error = read_chunk(
            &mut Cursor::new(&bytes),
            header,
            descriptor,
            &mut vec![0; MAX_TILE_BYTES],
            &check,
            |_| {
                canceled.set(true);
                Ok(())
            },
        )
        .err()
        .unwrap();
        assert!(error.contains("Canceled"));
        let error = read_chunk(
            &mut Cursor::new(&bytes),
            header,
            descriptor,
            &mut vec![0; MAX_TILE_BYTES],
            &|| Ok(()),
            |_| Err("Injected Store failure".into()),
        )
        .err()
        .unwrap();
        assert!(error.contains("Injected Store failure"));
    }

    #[test]
    fn standard_sha_binds_domains_lengths_and_exact_original_range_bytes() {
        let check = || Ok(());
        let a = standard_digest(&mut b"ab".as_slice(), 2, &mut b"c".as_slice(), 1, &check).unwrap();
        let b = standard_digest(&mut b"a".as_slice(), 1, &mut b"bc".as_slice(), 2, &check).unwrap();
        assert_ne!(a, b);
        let mut independent = b"PeerBrushSource36Standard\0".to_vec();
        independent.extend_from_slice(&2u64.to_be_bytes());
        independent.extend_from_slice(&1u64.to_be_bytes());
        independent.extend_from_slice(b"abc");
        assert_eq!(a, ring::digest::digest(&SHA256, &independent).as_ref());
        assert!(
            standard_digest(&mut b"abx".as_slice(), 2, &mut b"c".as_slice(), 1, &check).is_err()
        );
        assert!(standard_digest(&mut b"a".as_slice(), 2, &mut b"c".as_slice(), 1, &check).is_err());
    }

    #[test]
    fn short_writes_are_completed_and_write_failures_poison_the_chunk() {
        struct Short {
            bytes: Vec<u8>,
            remaining: Option<usize>,
        }
        impl Write for Short {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                let n = bytes.len().min(7).min(self.remaining.unwrap_or(usize::MAX));
                if n == 0 {
                    return Err(std::io::Error::other("Injected chunk write failure"));
                }
                self.bytes.extend_from_slice(&bytes[..n]);
                if let Some(left) = self.remaining.as_mut() {
                    *left -= n;
                }
                Ok(n)
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let spec = Spec::new(8).unwrap();
        let data = payload(spec);
        let check = || Ok(());
        let digest = tile_digest(spec, &data, &check).unwrap();
        let header = Header {
            ordinal: 0,
            total: 1,
            records: 1,
            core: [23; 32],
        };
        for remaining in [None, Some(HEADER as usize + 100)] {
            let mut output = Short {
                bytes: vec![],
                remaining,
            };
            let mut writer =
                ChunkWriter::begin(&mut output, header, HEADER + 48 + data.len() as u32, &check)
                    .unwrap();
            let result = writer.record(
                Record::Tile {
                    spec,
                    digest,
                    payload: &data,
                },
                &check,
            );
            if remaining.is_some() {
                assert!(result.unwrap_err().contains("Injected"));
                assert!(writer.finish(&check).is_err());
            } else {
                result.unwrap();
                let descriptor = writer.finish(&check).unwrap();
                read_chunk(
                    &mut Cursor::new(&output.bytes),
                    header,
                    descriptor,
                    &mut vec![0; MAX_TILE_BYTES],
                    &check,
                    |_| Ok(()),
                )
                .unwrap()
                .check(header, descriptor)
                .unwrap();
            }
        }
    }
}
