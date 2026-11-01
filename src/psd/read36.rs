//! Complete source byte/native-stream observations. Semantic feature admission,
//! typed metadata ownership, rendered layer/composite fidelity and final engine
//! publication remain separate. A provisional tile sink must not publish edits.
use super::{
    bulk36::{self, BulkLimits, BulkVerifier, CompletedBulk, Digest, Record, TileIndexEntry},
    manifest36::{CompletedStandard, Manifest},
    memory36::Admitted,
    row_stream::Source,
    structural::{Ambiguity, Encoding, Index, Span},
};
use ring::digest::{Context, SHA256};
use std::cell::Cell;
use std::io::{Read, Seek};

pub(crate) struct VerificationLimits {
    pub bulk: BulkLimits,
    pub native_streams: usize,
}

/// Whole-source SHA must come from the caller's retained source admission and
/// must bracket creation of this exact index. A directory scan is not admission.
#[allow(clippy::too_many_arguments)]
pub(crate) fn verify<'a, R: Read + Seek, T>(
    source: &'a Source<R>,
    index: &'a Index,
    expected_source_sha256: Digest,
    manifest: &Manifest<T>,
    limits: VerificationLimits,
    entries: &mut [TileIndexEntry],
    raster_counts: &mut [u64],
    scratch: &mut [u8],
    check: &dyn Fn() -> Result<(), String>,
    mut provisional_sink: impl FnMut(Record<'_>) -> Result<(), String>,
) -> Result<CompletedSource<'a, R>, String> {
    check()?;
    if scratch.len() != bulk36::MAX_TILE_BYTES
        || limits.native_streams == 0
        || limits.native_streams > 524289
        || index.file_bytes > 64 * 1024 * 1024 * 1024
        || index.header.channels != 4
        || index.header.width == 0
        || index.header.height == 0
        || index.header.width > 16000
        || index.header.height > 16000
        || ![8, 16].contains(&index.header.depth)
    {
        return Err("Source36 source/native stream admission differs".into());
    }
    let streams = index.layers.iter().try_fold(1usize, |count, layer| {
        count
            .checked_add(layer.channels.len())
            .ok_or("Source36 stream count overflow")
    })?;
    if streams > limits.native_streams {
        return Err("Source36 native stream count exceeds admission".into());
    }
    if index
        .ambiguities
        .iter()
        .any(|ambiguity| !matches!(ambiguity, Ambiguity::GlobalTag(key) if key == bulk36::KEY))
        || index
            .layers
            .iter()
            .any(|layer| layer.tags.iter().any(|tag| &tag.key == bulk36::KEY))
    {
        return Err("Source36 source has ambiguous or misplaced private structures".into());
    }
    let plan = manifest.plan();
    plan.admit(limits.bulk, check)?;
    let private_count = index
        .global_tags
        .iter()
        .filter(|tag| &tag.key == bulk36::KEY)
        .count();
    if private_count != plan.chunks.len() {
        return Err("Source36 source has missing or excess private chunks".into());
    }
    let mut resources = index.resources.iter().filter(|r| r.peerbrush_source);
    let resource = resources
        .next()
        .ok_or("Source36 source resource is missing")?;
    if resources.next().is_some() || resource.data.bytes < 10 {
        return Err("Source36 source resource is ambiguous or truncated".into());
    }
    let (encoded_bytes, encoded_sha) = manifest
        .resource_origin()
        .ok_or("Source36 manifest has no completed bounded source inflation")?;
    if encoded_bytes != resource.data.bytes - 4 {
        return Err("Source36 manifest resource length differs from its index".into());
    }
    source.unchanged_length()?;
    let whole = Span {
        offset: 0,
        bytes: index.file_bytes,
    };
    if hash_range(source, whole, scratch, check)? != expected_source_sha256 {
        return Err("Source36 source differs from its admitted whole-file SHA256".into());
    }
    let mut resource_input = source.range(resource.data, check)?;
    let mut marker = [0; 4];
    resource_input
        .read_exact(&mut marker)
        .map_err(|e| e.to_string())?;
    if &marker != b"PBR1"
        || hash_reader(&mut resource_input, encoded_bytes, scratch, check)? != encoded_sha
    {
        return Err("Source36 manifest resource changed after bounded inflation".into());
    }
    let standard = manifest.verify_standard(
        &mut source.range(index.layer_and_mask, check)?,
        index.layer_and_mask.bytes,
        &mut source.range(index.composite.span, check)?,
        index.composite.span.bytes,
        check,
    )?;
    let mut verifier = BulkVerifier::new(plan, limits.bulk, entries, raster_counts, check)?;
    for (ordinal, tag) in index
        .global_tags
        .iter()
        .filter(|tag| &tag.key == bulk36::KEY)
        .enumerate()
    {
        check()?;
        let descriptor = &plan.chunks[ordinal];
        if tag.signature != *b"8BIM" || tag.data.bytes != u64::from(descriptor.bytes) {
            return Err("Source36 private chunk signature or declared span differs".into());
        }
        let mut prefix = [0; 24];
        source
            .range(tag.data, check)?
            .read_exact(&mut prefix)
            .map_err(|e| e.to_string())?;
        let header = bulk36::Header {
            ordinal: ordinal as u32,
            total: plan.chunks.len() as u32,
            records: u32::from_be_bytes(prefix[20..24].try_into().unwrap()),
            core: plan.core,
        };
        verifier.chunk(
            &mut source.range(tag.data, check)?,
            header,
            scratch,
            check,
            &mut provisional_sink,
        )?;
    }
    let bulk = verifier.finish(check)?;
    let mut completed_streams = 0usize;
    let mut completed_rows = 0u64;
    for layer in &index.layers {
        for channel in &layer.channels {
            let encoding = channel
                .encoding
                .as_ref()
                .ok_or("Source36 standard channel has no validated native encoding")?;
            if channel.encoding_error.is_some() {
                return Err("Source36 standard channel has an invalid native encoding".into());
            }
            completed_rows = completed_rows
                .checked_add(finish_rows(source, encoding, scratch, check)?)
                .ok_or("Source36 completed row count overflow")?;
            completed_streams += 1;
        }
    }
    // Merged ZIP remains one linear channel-major stream. It is never treated
    // as independently seekable compressed channel streams here.
    completed_rows = completed_rows
        .checked_add(finish_rows(source, &index.composite, scratch, check)?)
        .ok_or("Source36 completed row count overflow")?;
    completed_streams += 1;
    if completed_streams != streams
        || hash_range(source, whole, scratch, check)? != expected_source_sha256
    {
        return Err("Source36 source changed during complete native-stream verification".into());
    }
    source.unchanged_length()?;
    check()?;
    Ok(CompletedSource {
        source,
        index,
        whole: expected_source_sha256,
        origin: (encoded_bytes, encoded_sha),
        bulk,
        standard,
        streams: completed_streams,
        rows: completed_rows,
        failed: Cell::new(false),
    })
}

fn finish_rows<R: Read + Seek>(
    source: &Source<R>,
    encoding: &Encoding,
    scratch: &mut [u8],
    check: &dyn Fn() -> Result<(), String>,
) -> Result<u64, String> {
    let row_bytes =
        usize::try_from(encoding.row_bytes).map_err(|_| "Source36 native row length overflow")?;
    if row_bytes > scratch.len() {
        return Err("Source36 native row exceeds admitted transfer scratch".into());
    }
    let mut rows = source.rows(encoding, check)?;
    for _ in 0..encoding.rows {
        check()?;
        if !rows.read_row(&mut scratch[..row_bytes])? {
            return Err("Source36 native row stream ended early".into());
        }
    }
    rows.finish()?.check(source, encoding, check)?;
    Ok(encoding.rows)
}
fn hash_range<R: Read + Seek>(
    source: &Source<R>,
    span: Span,
    scratch: &mut [u8],
    check: &dyn Fn() -> Result<(), String>,
) -> Result<Digest, String> {
    hash_reader(&mut source.range(span, check)?, span.bytes, scratch, check)
}
fn hash_reader(
    input: &mut impl Read,
    bytes: u64,
    scratch: &mut [u8],
    check: &dyn Fn() -> Result<(), String>,
) -> Result<Digest, String> {
    let mut hash = Context::new(&SHA256);
    let mut left = bytes;
    while left != 0 {
        check()?;
        let count = left.min(scratch.len().min(65536) as u64) as usize;
        input
            .read_exact(&mut scratch[..count])
            .map_err(|e| e.to_string())?;
        hash.update(&scratch[..count]);
        left -= count as u64;
    }
    let mut extra = [0; 1];
    if input.read(&mut extra).map_err(|e| e.to_string())? != 0 {
        return Err("Source36 source range contains excess bytes".into());
    }
    check()?;
    Ok(hash.finish().as_ref().try_into().unwrap())
}

/// This token exists only after required stream finish/checksum/end checks. It
/// cannot be reconstructed from requested row counts. Keeping exact source and
/// index borrows prevents replacing either with a same-shaped caller object.
#[must_use = "Source observations require typed/render/lease/publication validation"]
pub(crate) struct CompletedSource<'a, R> {
    source: &'a Source<R>,
    index: &'a Index,
    whole: Digest,
    origin: (u64, Digest),
    bulk: CompletedBulk,
    standard: CompletedStandard,
    streams: usize,
    rows: u64,
    failed: Cell<bool>,
}
impl<R: Read + Seek> CompletedSource<'_, R> {
    pub(crate) fn check<T>(
        &self,
        source: &Source<R>,
        index: &Index,
        manifest: &Manifest<T>,
        expected_whole: Digest,
        check: &dyn Fn() -> Result<(), String>,
    ) -> Result<(), String> {
        if self.failed.replace(true) {
            return Err("Source36 source completion is failed".into());
        }
        let result = (|| {
            check()?;
            if !std::ptr::eq(self.source, source)
                || !std::ptr::eq(self.index, index)
                || self.whole != expected_whole
                || manifest.resource_origin() != Some(self.origin)
            {
                return Err(
                    "Source36 completion belongs to another source/index/manifest admission".into(),
                );
            }
            self.bulk.check(manifest.plan(), check)?;
            self.standard.check(manifest)?;
            source.unchanged_length()?;
            check()
        })();
        if result.is_ok() {
            self.failed.set(false);
        }
        result
    }
    pub(crate) fn streams(&self) -> usize {
        self.streams
    }
    pub(crate) fn rows(&self) -> u64 {
        self.rows
    }
    /// Reobserve whole-file bytes before handoff. The caller separately rechecks
    /// the retained source file identity/lease and final engine revision guard.
    pub(crate) fn recheck<T>(
        &self,
        source: &Source<R>,
        index: &Index,
        manifest: &Manifest<T>,
        expected_whole: Digest,
        scratch: &mut [u8],
        check: &dyn Fn() -> Result<(), String>,
    ) -> Result<(), String> {
        self.check(source, index, manifest, expected_whole, check)?;
        self.failed.set(true);
        let result = (|| {
            if scratch.is_empty() || scratch.len() > bulk36::MAX_TILE_BYTES {
                return Err("Source36 source recheck requires admitted bounded scratch".into());
            }
            if hash_range(
                source,
                Span {
                    offset: 0,
                    bytes: index.file_bytes,
                },
                scratch,
                check,
            )? != self.whole
            {
                return Err("Source36 source changed after completed stream observations".into());
            }
            source.unchanged_length()?;
            check()
        })();
        if result.is_ok() {
            self.failed.set(false);
        }
        result
    }
    /// Consume the observed source completion and move the typed payload with
    /// its existing charge owner. This is the codec-to-PendingScene handoff,
    /// not permission to publish the scene without render/lease/revision checks.
    pub(crate) fn into_metadata<T>(
        self,
        source: &Source<R>,
        index: &Index,
        manifest: Admitted<Manifest<T>>,
        expected_whole: Digest,
        scratch: &mut [u8],
        check: &dyn Fn() -> Result<(), String>,
    ) -> Result<Admitted<T>, String> {
        self.recheck(source, index, &manifest, expected_whole, scratch, check)?;
        manifest.map_unique(|manifest, _| Ok(manifest.into_metadata()))
    }
}
