//! Append-only Photoshop container replay. Completion observes bytes, not a
//! current render, typed source admission, source lease or final publication.
use super::{
    bulk36::{CompletedBulk, Digest},
    canonical36::Limits,
    manifest36::{CompletedStandard, Manifest},
    structural::{Format, Header},
};
use ring::digest::{Context, SHA256};
use serde::Serialize;
use std::io::{Read, Write};

const PSD_MAX_BYTES: u64 = 2_000_000_000;
const MAX_CONTAINER_BYTES: u64 = 64 * 1024 * 1024 * 1024;
const TRANSFER: usize = 64 * 1024;

pub(crate) struct Profile<'a> {
    pub bytes: &'a [u8],
}

/// The caller's owned append sink must remain private until its own seal and
/// source checks finish. This token does not own that sink or grant path access.
pub(crate) struct CompressedManifest {
    core: Digest,
    canonical: Digest,
    bytes: u64,
    sha256: Digest,
}
impl CompressedManifest {
    pub(crate) fn bytes(&self) -> u64 {
        self.bytes
    }
    fn check<T: Serialize>(
        &self,
        manifest: &Manifest<T>,
        metadata: Limits,
        check: &dyn Fn() -> Result<(), String>,
    ) -> Result<(), String> {
        let canonical = manifest.canonical(metadata, check)?;
        if self.core != manifest.core().sha256() || self.canonical != digest(canonical.bytes()) {
            return Err("Compressed Source36 manifest belongs to different metadata".into());
        }
        Ok(())
    }
}

/// Canonical buffers and compressor/transfer storage must be included in the
/// caller's combined admission. No full-file or native-pixel buffer is made.
pub(crate) fn compress_manifest<T: Serialize, W: Write>(
    manifest: &Manifest<T>,
    metadata: Limits,
    output: &mut W,
    check: &dyn Fn() -> Result<(), String>,
) -> Result<CompressedManifest, String> {
    let canonical = manifest.canonical(metadata, check)?;
    let mut sink = HashWriter::new(output, u64::from(u32::MAX) - 4, check);
    {
        let mut encoder = flate2::write::ZlibEncoder::new(&mut sink, flate2::Compression::fast());
        for bytes in canonical.bytes().chunks(TRANSFER) {
            check()?;
            encoder.write_all(bytes).map_err(|e| e.to_string())?;
        }
        encoder.finish().map_err(|e| e.to_string())?;
    }
    sink.flush().map_err(|e| e.to_string())?;
    check()?;
    let (bytes, sha256) = sink.finish();
    Ok(CompressedManifest {
        core: manifest.core().sha256(),
        canonical: digest(canonical.bytes()),
        bytes,
        sha256,
    })
}

/// Computes every length before output is touched, including Adobe's PSD size
/// limit and PSB's still-32-bit image-resource section. Larger output requires a
/// separate admitted container/index design; it is not a new live canvas cap.
#[derive(Debug)]
pub(crate) struct LayoutPlan {
    header: Header,
    resources: u32,
    layer_bytes: u64,
    composite_bytes: u64,
    output_bytes: u64,
}
impl LayoutPlan {
    pub(crate) fn new(
        header: Header,
        layer_bytes: u64,
        composite_bytes: u64,
        compressed_manifest_bytes: u64,
        profile: Option<&Profile<'_>>,
    ) -> Result<Self, String> {
        if header.channels != 4
            || ![8, 16].contains(&header.depth)
            || header.width == 0
            || header.height == 0
            || header.width > 16000
            || header.height > 16000
            || layer_bytes == 0
            || composite_bytes < 2
            || compressed_manifest_bytes < 6
        {
            return Err("Source36 container geometry or section declarations differ".into());
        }
        let payload = compressed_manifest_bytes
            .checked_add(4)
            .and_then(|n| u32::try_from(n).ok())
            .ok_or("Source36 image-resource payload overflows its 32-bit field")?;
        // Signature, ID, padded Pascal name, length, PBR1+zlib and even padding.
        let mut resources = 24u64 + u64::from(payload) + u64::from(payload % 2);
        if let Some(profile) = profile {
            if profile.bytes.is_empty() || profile.bytes.len() > 4 * 1024 * 1024 {
                return Err("Source36 profile exceeds the existing ICC byte envelope".into());
            }
            resources = resources
                .checked_add(12 + profile.bytes.len() as u64 + profile.bytes.len() as u64 % 2)
                .ok_or("Source36 profile resource length overflow")?;
        }
        let resources = u32::try_from(resources)
            .map_err(|_| "Source36 image resources overflow their 32-bit field")?;
        let large = header.format == Format::Psb;
        if !large && layer_bytes > u64::from(u32::MAX) {
            return Err("PSD layer section requires PSB's 64-bit length".into());
        }
        let output_bytes = (26u64 + 4 + 4 + u64::from(resources) + if large { 8 } else { 4 })
            .checked_add(layer_bytes)
            .and_then(|n| n.checked_add(composite_bytes))
            .ok_or("Source36 container length overflow")?;
        if output_bytes > MAX_CONTAINER_BYTES {
            return Err("Source36 container exceeds the admitted 64 GiB spool envelope".into());
        }
        if !large && output_bytes > PSD_MAX_BYTES {
            return Err("Photoshop output exceeds PSD's 2 GB limit; choose PSB".into());
        }
        Ok(Self {
            header,
            resources,
            layer_bytes,
            composite_bytes,
            output_bytes,
        })
    }
    pub(crate) fn output_bytes(&self) -> u64 {
        self.output_bytes
    }
}

/// Replays only read-only completed source ranges into an owned append sink.
/// Readers must be bounded to precisely their named range; encoded tails and
/// short reads fail. Standard SHA is checked again during this exact replay.
#[allow(clippy::too_many_arguments)]
pub(crate) fn write<T: Serialize, W: Write, M: Read, L: Read, C: Read>(
    output: &mut W,
    header: Header,
    manifest: &Manifest<T>,
    metadata: Limits,
    compressed: &CompressedManifest,
    compressed_input: &mut M,
    standard: &CompletedStandard,
    bulk: &CompletedBulk,
    layers: &mut L,
    composite: &mut C,
    profile: Option<&Profile<'_>>,
    scratch: &mut [u8],
    check: &dyn Fn() -> Result<(), String>,
) -> Result<CompletedContainer, String> {
    check()?;
    let (layer_bytes, composite_bytes, standard_sha) = manifest.standard_binding();
    let plan = LayoutPlan::new(
        header,
        layer_bytes,
        composite_bytes,
        compressed.bytes,
        profile,
    )?;
    if scratch.is_empty() || scratch.len() > TRANSFER {
        return Err("Source36 replay requires admitted bounded transfer scratch".into());
    }
    compressed.check(manifest, metadata, check)?;
    standard.check(manifest)?;
    bulk.check(manifest.plan(), check)?;
    let mut out = HashWriter::new(output, plan.output_bytes, check);
    out.write_all(b"8BPS").map_err(|e| e.to_string())?;
    out.write_all(
        &(if header.format == Format::Psb {
            2u16
        } else {
            1
        })
        .to_be_bytes(),
    )
    .map_err(|e| e.to_string())?;
    out.write_all(&[0; 6]).map_err(|e| e.to_string())?;
    out.write_all(&4u16.to_be_bytes())
        .map_err(|e| e.to_string())?;
    out.write_all(&header.height.to_be_bytes())
        .map_err(|e| e.to_string())?;
    out.write_all(&header.width.to_be_bytes())
        .map_err(|e| e.to_string())?;
    out.write_all(&header.depth.to_be_bytes())
        .map_err(|e| e.to_string())?;
    out.write_all(&3u16.to_be_bytes())
        .map_err(|e| e.to_string())?;
    out.write_all(&0u32.to_be_bytes())
        .map_err(|e| e.to_string())?;
    out.write_all(&plan.resources.to_be_bytes())
        .map_err(|e| e.to_string())?;
    out.write_all(b"8BIM").map_err(|e| e.to_string())?;
    out.write_all(&4000u16.to_be_bytes())
        .map_err(|e| e.to_string())?;
    out.write_all(b"\x0cPeerBrush.v1\0")
        .map_err(|e| e.to_string())?;
    let payload = (compressed.bytes + 4) as u32;
    out.write_all(&payload.to_be_bytes())
        .map_err(|e| e.to_string())?;
    out.write_all(b"PBR1").map_err(|e| e.to_string())?;
    if copy_exact(
        compressed_input,
        &mut out,
        compressed.bytes,
        scratch,
        None,
        check,
    )? != compressed.sha256
    {
        return Err("Source36 compressed manifest changed during replay".into());
    }
    if payload % 2 != 0 {
        out.write_all(&[0]).map_err(|e| e.to_string())?;
    }
    if let Some(profile) = profile {
        out.write_all(b"8BIM").map_err(|e| e.to_string())?;
        out.write_all(&1039u16.to_be_bytes())
            .map_err(|e| e.to_string())?;
        out.write_all(&[0; 2]).map_err(|e| e.to_string())?;
        out.write_all(&(profile.bytes.len() as u32).to_be_bytes())
            .map_err(|e| e.to_string())?;
        for bytes in profile.bytes.chunks(TRANSFER) {
            out.write_all(bytes).map_err(|e| e.to_string())?;
        }
        if profile.bytes.len() % 2 != 0 {
            out.write_all(&[0]).map_err(|e| e.to_string())?;
        }
    }
    if header.format == Format::Psb {
        out.write_all(&plan.layer_bytes.to_be_bytes())
            .map_err(|e| e.to_string())?;
    } else {
        out.write_all(&(plan.layer_bytes as u32).to_be_bytes())
            .map_err(|e| e.to_string())?;
    }
    let mut binding = Context::new(&SHA256);
    binding.update(b"PeerBrushSource36Standard\0");
    binding.update(&plan.layer_bytes.to_be_bytes());
    binding.update(&plan.composite_bytes.to_be_bytes());
    copy_exact(
        layers,
        &mut out,
        plan.layer_bytes,
        scratch,
        Some(&mut binding),
        check,
    )?;
    copy_exact(
        composite,
        &mut out,
        plan.composite_bytes,
        scratch,
        Some(&mut binding),
        check,
    )?;
    if finish(binding) != standard_sha {
        return Err("Source36 original standard bytes changed during replay".into());
    }
    out.flush().map_err(|e| e.to_string())?;
    check()?;
    let (bytes, sha256) = out.finish();
    if bytes != plan.output_bytes {
        return Err("Source36 assembled container length differs".into());
    }
    Ok(CompletedContainer {
        bytes,
        sha256,
        canonical: compressed.canonical,
        header,
    })
}

#[must_use = "Container bytes still require owned-file seal and final source publication checks"]
pub(crate) struct CompletedContainer {
    bytes: u64,
    sha256: Digest,
    canonical: Digest,
    header: Header,
}
impl CompletedContainer {
    pub(crate) fn bytes(&self) -> u64 {
        self.bytes
    }
    pub(crate) fn sha256(&self) -> Digest {
        self.sha256
    }
    pub(crate) fn check<T: Serialize>(
        &self,
        manifest: &Manifest<T>,
        metadata: Limits,
        header: Header,
        check: &dyn Fn() -> Result<(), String>,
    ) -> Result<(), String> {
        if self.canonical != digest(manifest.canonical(metadata, check)?.bytes())
            || self.header != header
        {
            return Err("Source36 container completion belongs to another source or header".into());
        }
        Ok(())
    }
}

fn digest(bytes: &[u8]) -> Digest {
    ring::digest::digest(&SHA256, bytes)
        .as_ref()
        .try_into()
        .unwrap()
}
fn finish(context: Context) -> Digest {
    context.finish().as_ref().try_into().unwrap()
}
fn copy_exact<R: Read, W: Write>(
    input: &mut R,
    output: &mut W,
    bytes: u64,
    scratch: &mut [u8],
    mut binding: Option<&mut Context>,
    check: &dyn Fn() -> Result<(), String>,
) -> Result<Digest, String> {
    let mut remaining = bytes;
    let mut hash = Context::new(&SHA256);
    while remaining != 0 {
        check()?;
        let count = remaining.min(scratch.len() as u64) as usize;
        input
            .read_exact(&mut scratch[..count])
            .map_err(|e| e.to_string())?;
        output
            .write_all(&scratch[..count])
            .map_err(|e| e.to_string())?;
        hash.update(&scratch[..count]);
        if let Some(binding) = &mut binding {
            binding.update(&scratch[..count]);
        }
        remaining -= count as u64;
    }
    check()?;
    let mut extra = [0; 1];
    if input.read(&mut extra).map_err(|e| e.to_string())? != 0 {
        return Err("Source36 replay range contains excess bytes".into());
    }
    Ok(finish(hash))
}

struct HashWriter<'a, W> {
    output: W,
    limit: u64,
    written: u64,
    sha: Context,
    check: &'a dyn Fn() -> Result<(), String>,
    failed: bool,
}
impl<'a, W: Write> HashWriter<'a, W> {
    fn new(output: W, limit: u64, check: &'a dyn Fn() -> Result<(), String>) -> Self {
        Self {
            output,
            limit,
            written: 0,
            sha: Context::new(&SHA256),
            check,
            failed: false,
        }
    }
    fn finish(self) -> (u64, Digest) {
        (self.written, finish(self.sha))
    }
}
impl<W: Write> Write for HashWriter<'_, W> {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if self.failed {
            return Err(std::io::Error::other("Source36 append sink failed"));
        }
        self.failed = true;
        (self.check)().map_err(std::io::Error::other)?;
        let end = self
            .written
            .checked_add(bytes.len() as u64)
            .filter(|n| *n <= self.limit)
            .ok_or_else(|| std::io::Error::other("Source36 append sink exceeds exact admission"))?;
        self.output.write_all(bytes)?;
        self.sha.update(bytes);
        self.written = end;
        self.failed = false;
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        if self.failed {
            return Err(std::io::Error::other("Source36 append sink failed"));
        }
        self.failed = true;
        (self.check)().map_err(std::io::Error::other)?;
        self.output.flush()?;
        self.failed = false;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::psd::{
        bulk36::{
            self, BulkLimits, BulkVerifier, ChunkWriter, Record, Reference, Spec, TileIndexEntry,
        },
        manifest36::{self, Core, Raster, StagedCore},
        row_stream::Source,
        structural::{self, Span},
    };
    use serde_json::json;
    use std::io::Cursor;

    fn metadata() -> Limits {
        Limits {
            bytes: 16384,
            nodes: 4096,
            depth: 32,
        }
    }
    fn limits() -> BulkLimits {
        BulkLimits {
            rasters: 4,
            tiles: 4,
            references: 4,
            chunks: 4,
            encoded_bytes: 8 * 1024 * 1024,
        }
    }
    fn range(bytes: &[u8], span: Span) -> &[u8] {
        &bytes[span.offset as usize..span.end().unwrap() as usize]
    }
    fn root(domain: &[u8], rows: &[Vec<u8>]) -> String {
        let mut context = Context::new(&SHA256);
        context.update(domain);
        context.update(&(rows.len() as u64).to_be_bytes());
        for row in rows {
            context.update(row);
        }
        manifest36::hex(finish(context))
    }
    struct Fixture {
        header: Header,
        manifest: Manifest,
        layer: Vec<u8>,
        composite: Vec<u8>,
        original: Vec<u8>,
        chunks: Vec<(bulk36::Header, Vec<u8>)>,
    }
    fn fixture(format: Format, depth: u16) -> Fixture {
        let check = || Ok(());
        let original = structural::tests::fixture(format, depth, false);
        let index = structural::index(
            &mut Cursor::new(&original),
            structural::Limits::current(),
            || Ok(()),
        )
        .unwrap();
        let spec = Spec::new(depth).unwrap();
        let mut tiles = vec![];
        let mut references = vec![];
        for tile_y in 0..2u32 {
            let mut tile = vec![0; spec.bytes()];
            for yy in 0..256u32 {
                let y = tile_y * 256 + yy;
                if y >= 257 {
                    break;
                }
                for x in 0..3u32 {
                    for c in 0..4u32 {
                        let value = if c == 3 {
                            u16::MAX
                        } else {
                            (10001 + x * 173 + y * 211 + c * 131) as u16
                        };
                        let i = (yy as usize * 256 * 4 + x as usize * 4 + c as usize)
                            * usize::from(depth / 8);
                        if depth == 16 {
                            tile[i..i + 2].copy_from_slice(&value.to_be_bytes());
                        } else {
                            tile[i] = (value / 257) as u8;
                        }
                    }
                }
            }
            let hash = bulk36::tile_digest(spec, &tile, &check).unwrap();
            references.push(Reference {
                raster: 1,
                x: 0,
                y: tile_y,
                tile: hash,
            });
            tiles.push((hash, tile));
        }
        tiles.sort_unstable_by_key(|t| t.0);
        let tile_rows = tiles
            .iter()
            .map(|(hash, tile)| {
                let mut wire = vec![1, 0, 1, 0];
                wire.extend_from_slice(&depth.to_be_bytes());
                wire.extend_from_slice(&4u16.to_be_bytes());
                wire.extend_from_slice(hash);
                wire.extend_from_slice(&(tile.len() as u32).to_be_bytes());
                wire
            })
            .collect::<Vec<_>>();
        let ref_rows = references
            .iter()
            .map(|r| {
                let mut wire = r.raster.to_be_bytes().to_vec();
                wire.extend_from_slice(&r.x.to_be_bytes());
                wire.extend_from_slice(&r.y.to_be_bytes());
                wire.extend_from_slice(&r.tile);
                wire
            })
            .collect::<Vec<_>>();
        let core = StagedCore::new(
            Core {
                format: 36,
                source_id: "61f53110-3789-4693-81d7-56136f750be2".into(),
                revision: 7,
                metadata: json!({"scope":"container framing fixture"}),
                rasters: vec![Raster {
                    id: 1,
                    width: 3,
                    height: 257,
                    depth,
                    references: 2,
                }],
                tiles: 2,
                references: 2,
                tile_root: root(b"PeerBrushSource36Tiles\0", &tile_rows),
                reference_root: root(b"PeerBrushSource36Refs\0", &ref_rows),
            },
            metadata(),
            limits(),
            &check,
        )
        .unwrap();
        let mut chunks = vec![];
        let mut descriptors = vec![];
        let mut layer = range(&original, index.layer_and_mask).to_vec();
        for ordinal in 0..2 {
            let header = bulk36::Header {
                ordinal,
                total: 2,
                records: 2,
                core: core.sha256(),
            };
            let records = if ordinal == 0 {
                tiles
                    .iter()
                    .map(|(digest, payload)| Record::Tile {
                        spec,
                        digest: *digest,
                        payload,
                    })
                    .collect::<Vec<_>>()
            } else {
                references
                    .iter()
                    .copied()
                    .map(Record::Reference)
                    .collect::<Vec<_>>()
            };
            let expected = 56
                + records
                    .iter()
                    .map(|r| match r {
                        Record::Tile { payload, .. } => 48 + payload.len() as u32,
                        Record::Reference(_) => 48,
                    })
                    .sum::<u32>();
            let mut bytes = vec![];
            let mut writer = ChunkWriter::begin(&mut bytes, header, expected, &check).unwrap();
            for record in records {
                writer.record(record, &check).unwrap();
            }
            descriptors.push(writer.finish(&check).unwrap());
            layer.extend_from_slice(b"8BIMPb36");
            // Pb36 is an ordinary32-bit tag even in a PSB layer section.
            layer.extend_from_slice(&(bytes.len() as u32).to_be_bytes());
            layer.extend_from_slice(&bytes);
            layer.resize(layer.len() + (4 - bytes.len() % 4) % 4, 0);
            chunks.push((header, bytes));
        }
        let composite = range(&original, index.composite.span).to_vec();
        let standard = bulk36::standard_digest(
            &mut layer.as_slice(),
            layer.len() as u64,
            &mut composite.as_slice(),
            composite.len() as u64,
            &check,
        )
        .unwrap();
        let manifest = Manifest::bind(
            core,
            layer.len() as u64,
            composite.len() as u64,
            standard,
            descriptors,
            limits(),
            &check,
        )
        .unwrap();
        Fixture {
            header: index.header,
            manifest,
            layer,
            composite,
            original,
            chunks,
        }
    }
    fn proofs(f: &Fixture) -> (CompletedBulk, CompletedStandard) {
        let check = || Ok(());
        let mut entries = [TileIndexEntry::vacant(); 2];
        let mut counters = [0];
        let mut verifier = BulkVerifier::new(
            f.manifest.plan(),
            limits(),
            &mut entries,
            &mut counters,
            &check,
        )
        .unwrap();
        let mut scratch = vec![0; bulk36::MAX_TILE_BYTES];
        for (header, bytes) in &f.chunks {
            verifier
                .chunk(&mut bytes.as_slice(), *header, &mut scratch, &check, |_| {
                    Ok(())
                })
                .unwrap();
        }
        let bulk = verifier.finish(&check).unwrap();
        let standard = f
            .manifest
            .verify_standard(
                &mut f.layer.as_slice(),
                f.layer.len() as u64,
                &mut f.composite.as_slice(),
                f.composite.len() as u64,
                &check,
            )
            .unwrap();
        (bulk, standard)
    }
    fn encoded(f: &Fixture) -> (Vec<u8>, CompressedManifest) {
        let mut bytes = vec![];
        let token = compress_manifest(&f.manifest, metadata(), &mut bytes, &|| Ok(())).unwrap();
        (bytes, token)
    }
    #[test]
    fn native_multistrip_psd_psb_container_replay_preserves_standard_rows_and_complete_bulk() {
        let check = || Ok(());
        for format in [Format::Psd, Format::Psb] {
            for depth in [8, 16] {
                let f = fixture(format, depth);
                let (bulk, standard) = proofs(&f);
                let (encoded, compressed) = encoded(&f);
                assert_eq!(compressed.bytes(), encoded.len() as u64);
                let profile = Profile {
                    bytes: b"profile framing observation",
                };
                let plan = LayoutPlan::new(
                    f.header,
                    f.layer.len() as u64,
                    f.composite.len() as u64,
                    compressed.bytes(),
                    Some(&profile),
                )
                .unwrap();
                let mut output = vec![];
                let completion = write(
                    &mut output,
                    f.header,
                    &f.manifest,
                    metadata(),
                    &compressed,
                    &mut encoded.as_slice(),
                    &standard,
                    &bulk,
                    &mut f.layer.as_slice(),
                    &mut f.composite.as_slice(),
                    Some(&profile),
                    &mut [0; 65536],
                    &check,
                )
                .unwrap();
                assert_eq!(output.len() as u64, plan.output_bytes());
                assert_eq!(completion.bytes(), output.len() as u64);
                assert_eq!(completion.sha256(), digest(&output));
                completion
                    .check(&f.manifest, metadata(), f.header, &check)
                    .unwrap();
                assert!(completion
                    .check(
                        &f.manifest,
                        metadata(),
                        Header {
                            width: 4,
                            ..f.header
                        },
                        &check
                    )
                    .is_err());
                let mut changed = f
                    .manifest
                    .canonical(metadata(), &check)
                    .unwrap()
                    .value()
                    .clone();
                changed["standard"]["sha256"] = json!("00".repeat(32));
                let changed =
                    crate::psd::canonical36::Canonical::from_value(changed, metadata(), &check)
                        .unwrap();
                let changed =
                    Manifest::parse(changed.bytes(), metadata(), limits(), &check).unwrap();
                assert_eq!(changed.core().sha256(), f.manifest.core().sha256());
                assert!(completion
                    .check(&changed, metadata(), f.header, &check)
                    .is_err());
                let parsed = structural::index(
                    &mut Cursor::new(&output),
                    structural::Limits::current(),
                    || Ok(()),
                )
                .unwrap();
                assert_eq!(parsed.header, f.header);
                assert_eq!(range(&output, parsed.layer_and_mask), f.layer);
                assert_eq!(range(&output, parsed.composite.span), f.composite);
                let chunks = parsed
                    .global_tags
                    .iter()
                    .filter(|t| t.key == *b"Pb36")
                    .collect::<Vec<_>>();
                assert_eq!(chunks.len(), 2);
                for (tag, (_, bytes)) in chunks.iter().zip(&f.chunks) {
                    assert_eq!(range(&output, tag.data), bytes);
                }
                let resource = parsed
                    .resources
                    .iter()
                    .find(|r| r.peerbrush_source)
                    .unwrap();
                let bytes = range(&output, resource.data);
                assert_eq!(&bytes[..4], b"PBR1");
                let restored = manifest36::inflate(
                    &mut &bytes[4..],
                    (bytes.len() - 4) as u64,
                    metadata(),
                    limits(),
                    &check,
                )
                .unwrap();
                assert_eq!(restored.core().sha256(), f.manifest.core().sha256());
                let source = Source::new(Cursor::new(&output), output.len() as u64).unwrap();
                let mut entries = [TileIndexEntry::vacant(); 2];
                let mut counters = [0];
                let mut scratch = vec![0; bulk36::MAX_TILE_BYTES];
                let mut staged_tiles = 0;
                let source_proof = crate::psd::read36::verify(
                    &source,
                    &parsed,
                    digest(&output),
                    &restored,
                    crate::psd::read36::VerificationLimits {
                        bulk: limits(),
                        native_streams: 5,
                    },
                    &mut entries,
                    &mut counters,
                    &mut scratch,
                    &check,
                    |record| {
                        if matches!(record, Record::Tile { .. }) {
                            staged_tiles += 1;
                        }
                        Ok(())
                    },
                )
                .unwrap();
                assert_eq!(staged_tiles, 2);
                assert_eq!(source_proof.streams(), 5);
                assert_eq!(source_proof.rows(), 257 * 8);
                source_proof
                    .recheck(
                        &source,
                        &parsed,
                        &restored,
                        digest(&output),
                        &mut scratch,
                        &check,
                    )
                    .unwrap();
                assert!(source_proof
                    .check(&source, &parsed.clone(), &restored, digest(&output), &check)
                    .is_err());
                let other = Source::new(Cursor::new(&output), output.len() as u64).unwrap();
                assert!(source_proof
                    .check(&other, &parsed, &restored, digest(&output), &check)
                    .is_err());
                assert!(source_proof
                    .check(&source, &parsed, &f.manifest, digest(&output), &check)
                    .is_err());
                let old = structural::index(
                    &mut Cursor::new(&f.original),
                    structural::Limits::current(),
                    || Ok(()),
                )
                .unwrap();
                let new_source = Source::new(Cursor::new(&output), output.len() as u64).unwrap();
                let old_source =
                    Source::new(Cursor::new(&f.original), f.original.len() as u64).unwrap();
                for (new, old) in parsed.layers[0]
                    .channels
                    .iter()
                    .zip(&old.layers[0].channels)
                {
                    let mut new = new_source
                        .rows(new.encoding.as_ref().unwrap(), &check)
                        .unwrap();
                    let mut old = old_source
                        .rows(old.encoding.as_ref().unwrap(), &check)
                        .unwrap();
                    let mut a = vec![0; 3 * usize::from(depth / 8)];
                    let mut b = a.clone();
                    for _ in 0..257 {
                        assert!(new.read_row(&mut a).unwrap());
                        assert!(old.read_row(&mut b).unwrap());
                        assert_eq!(a, b);
                    }
                    new.finish().unwrap();
                    old.finish().unwrap();
                }
            }
        }
    }
    #[test]
    fn replay_refuses_changed_truncated_or_extra_ranges_and_cancelled_or_short_sinks() {
        let f = fixture(Format::Psb, 16);
        let (bulk, standard) = proofs(&f);
        let (encoded, compressed) = encoded(&f);
        for changed in 0..4 {
            let mut layer = f.layer.clone();
            let mut z = encoded.clone();
            match changed {
                0 => layer[20] ^= 1,
                1 => {
                    layer.pop();
                }
                2 => layer.push(0),
                _ => z[0] ^= 1,
            }
            let mut output = vec![];
            assert!(write(
                &mut output,
                f.header,
                &f.manifest,
                metadata(),
                &compressed,
                &mut z.as_slice(),
                &standard,
                &bulk,
                &mut layer.as_slice(),
                &mut f.composite.as_slice(),
                None,
                &mut [0; 65536],
                &|| Ok(())
            )
            .is_err());
        }
        let mut output = vec![];
        assert!(write(
            &mut output,
            f.header,
            &f.manifest,
            metadata(),
            &compressed,
            &mut encoded.as_slice(),
            &standard,
            &bulk,
            &mut f.layer.as_slice(),
            &mut f.composite.as_slice(),
            None,
            &mut [0; 65536],
            &|| Err("Cancelled".into())
        )
        .is_err());
        assert!(output.is_empty());
        struct Short {
            limit: usize,
            bytes: Vec<u8>,
        }
        impl Write for Short {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                let n = bytes
                    .len()
                    .min(7)
                    .min(self.limit.saturating_sub(self.bytes.len()));
                self.bytes.extend_from_slice(&bytes[..n]);
                Ok(n)
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let mut output = Short {
            limit: 1000,
            bytes: vec![],
        };
        assert!(write(
            &mut output,
            f.header,
            &f.manifest,
            metadata(),
            &compressed,
            &mut encoded.as_slice(),
            &standard,
            &bulk,
            &mut f.layer.as_slice(),
            &mut f.composite.as_slice(),
            None,
            &mut [0; 65536],
            &|| Ok(())
        )
        .is_err());
        assert_eq!(output.bytes.len(), 1000);
    }
    #[test]
    fn psd_size_resource_width_overflow_and_invalid_geometry_refuse_before_output() {
        let header = Header {
            format: Format::Psd,
            width: 16000,
            height: 16000,
            depth: 16,
            channels: 4,
        };
        assert!(
            LayoutPlan::new(header, 1_000_000_000, 1_000_000_000, 32, None)
                .unwrap_err()
                .contains("PSB")
        );
        assert!(LayoutPlan::new(header, u64::from(u32::MAX) + 1, 2, 32, None).is_err());
        let header = Header {
            format: Format::Psb,
            ..header
        };
        assert!(LayoutPlan::new(header, 4_000_000_000, 2_048_000_002, 32, None).is_ok());
        assert!(LayoutPlan::new(header, 1, 2, u64::from(u32::MAX), None).is_err());
        assert!(LayoutPlan::new(header, u64::MAX, 2, 32, None).is_err());
        assert!(LayoutPlan::new(Header { width: 0, ..header }, 1, 2, 32, None).is_err());
        assert!(LayoutPlan::new(
            Header {
                depth: 32,
                ..header
            },
            1,
            2,
            32,
            None
        )
        .is_err());
    }

    #[test]
    fn complete_source_requires_bound_inflation_and_rejects_changed_ranges_before_staging() {
        use crate::psd::read36::{self, VerificationLimits};
        let check = || Ok(());
        let f = fixture(Format::Psb, 16);
        let (bulk, standard) = proofs(&f);
        let (encoded, compressed) = encoded(&f);
        let mut output = vec![];
        write(
            &mut output,
            f.header,
            &f.manifest,
            metadata(),
            &compressed,
            &mut encoded.as_slice(),
            &standard,
            &bulk,
            &mut f.layer.as_slice(),
            &mut f.composite.as_slice(),
            None,
            &mut [0; 65536],
            &check,
        )
        .unwrap();
        let index = structural::index(
            &mut Cursor::new(&output),
            structural::Limits::current(),
            || Ok(()),
        )
        .unwrap();
        let resource = index.resources.iter().find(|r| r.peerbrush_source).unwrap();
        let resource_bytes = range(&output, resource.data);
        let manifest = manifest36::inflate(
            &mut &resource_bytes[4..],
            (resource_bytes.len() - 4) as u64,
            metadata(),
            limits(),
            &check,
        )
        .unwrap();
        let mut entries = [TileIndexEntry::vacant(); 2];
        let mut counts = [0];
        let mut scratch = vec![0; bulk36::MAX_TILE_BYTES];
        for variant in 0..4 {
            let mut changed = output.clone();
            match variant {
                0 => changed[resource.data.offset as usize + 5] ^= 1,
                1 => changed[index.composite.span.offset as usize + 4] ^= 1,
                2 => changed[index.layer_and_mask.offset as usize + 20] ^= 1,
                _ => (),
            }
            let source = Source::new(Cursor::new(&changed), changed.len() as u64).unwrap();
            let mut staged = 0;
            let supplied = if variant == 3 { &f.manifest } else { &manifest };
            assert!(read36::verify(
                &source,
                &index,
                digest(&changed),
                supplied,
                VerificationLimits {
                    bulk: limits(),
                    native_streams: 5
                },
                &mut entries,
                &mut counts,
                &mut scratch,
                &check,
                |_| {
                    staged += 1;
                    Ok(())
                }
            )
            .is_err());
            assert_eq!(staged, 0);
        }
        struct MutableInput {
            bytes: std::rc::Rc<std::cell::RefCell<Vec<u8>>>,
            position: usize,
        }
        impl Read for MutableInput {
            fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
                let bytes = self.bytes.borrow();
                let n = out.len().min(bytes.len().saturating_sub(self.position));
                out[..n].copy_from_slice(&bytes[self.position..self.position + n]);
                self.position += n;
                Ok(n)
            }
        }
        impl std::io::Seek for MutableInput {
            fn seek(&mut self, from: std::io::SeekFrom) -> std::io::Result<u64> {
                let length = self.bytes.borrow().len() as i128;
                let position = match from {
                    std::io::SeekFrom::Start(n) => i128::from(n),
                    std::io::SeekFrom::Current(n) => self.position as i128 + i128::from(n),
                    std::io::SeekFrom::End(n) => length + i128::from(n),
                };
                if !(0..=length).contains(&position) {
                    return Err(std::io::Error::other("outside mutable fixture"));
                }
                self.position = position as usize;
                Ok(position as u64)
            }
        }
        let sha = digest(&output);
        let bytes = std::rc::Rc::new(std::cell::RefCell::new(output));
        let source = Source::new(
            MutableInput {
                bytes: bytes.clone(),
                position: 0,
            },
            index.file_bytes,
        )
        .unwrap();
        let proof = read36::verify(
            &source,
            &index,
            sha,
            &manifest,
            VerificationLimits {
                bulk: limits(),
                native_streams: 5,
            },
            &mut entries,
            &mut counts,
            &mut scratch,
            &check,
            |_| Ok(()),
        )
        .unwrap();
        bytes.borrow_mut()[index.composite.span.offset as usize + 4] ^= 1;
        assert!(proof
            .recheck(&source, &index, &manifest, sha, &mut scratch, &check)
            .is_err());
    }
}
