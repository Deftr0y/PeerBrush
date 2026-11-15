//! Complete source byte/native-stream observations. Semantic feature admission,
//! typed metadata ownership, rendered layer/composite fidelity and final engine
//! publication remain separate. A provisional tile sink must not publish edits.
use super::{
    bulk36::{self, BulkLimits, BulkVerifier, CompletedBulk, Digest, Record, TileIndexEntry},
    manifest36::{CompletedStandard, Manifest},
    memory36::Admitted,
    row_stream::Source,
    structural::{Ambiguity, Compression, Encoding, Index, Span},
};
use ring::digest::{Context, SHA256};
use std::cell::Cell;
use std::io::{Read, Seek};
use std::sync::Arc;

pub(crate) struct VerificationLimits {
    pub bulk: BulkLimits,
    pub native_streams: usize,
}

pub(crate) enum VerificationPhase {
    InitialChecksum,
    EditableSources { total_bytes: u64 },
    Streams { total: usize },
    AfterStreamsChecksum,
}

/// Whole-source SHA must come from the caller's retained source admission and
/// must bracket creation of this exact index. A directory scan is not admission.
#[allow(clippy::too_many_arguments)]
pub(crate) fn verify<'source, 'index, R: Read + Seek, T>(
    source: &'source Source<R>,
    index: &'index Index,
    expected_source_sha256: Digest,
    manifest: &Manifest<T>,
    limits: VerificationLimits,
    entries: &mut [TileIndexEntry],
    raster_counts: &mut [u64],
    scratch: &mut [u8],
    checks: (
        &dyn Fn() -> Result<(), String>,
        &dyn Fn() -> Result<(), String>,
    ),
    mut progress: impl FnMut(VerificationPhase, u64) -> Result<(), String>,
    mut provisional_sink: impl FnMut(Record<'_>) -> Result<(), String>,
) -> Result<CompletedSource<'source, 'index, R>, String> {
    let (check, pinned) = checks;
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
    let layer = index.layer_and_mask;
    let composite = index.composite.span;
    let (whole_sha, standard_sha) =
        hash_source_and_standard(source, whole, [layer, composite], scratch, check, |bytes| {
            progress(VerificationPhase::InitialChecksum, bytes)
        })?;
    if whole_sha != expected_source_sha256 {
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
    let standard = manifest.verify_standard_digest(layer.bytes, composite.bytes, standard_sha)?;
    let mut verifier = BulkVerifier::new(plan, limits.bulk, entries, raster_counts, check)?;
    let total_bytes = plan.chunks.iter().map(|chunk| u64::from(chunk.bytes)).sum();
    let mut verified_bytes = 0;
    if total_bytes != 0 {
        progress(VerificationPhase::EditableSources { total_bytes }, 0)?;
    }
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
            &mut source.range_with_checks(tag.data, check, pinned)?,
            header,
            scratch,
            pinned,
            &mut provisional_sink,
        )?;
        check()?;
        verified_bytes += tag.data.bytes;
        progress(VerificationPhase::EditableSources { total_bytes }, verified_bytes)?;
    }
    let bulk = verifier.finish(check)?;
    progress(VerificationPhase::Streams { total: streams }, 0)?;
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
                .checked_add(verify_stream(source, encoding, scratch, check, pinned)?)
                .ok_or("Source36 completed row count overflow")?;
            completed_streams += 1;
            progress(VerificationPhase::Streams { total: streams }, completed_streams as u64)?;
        }
    }
    // Merged ZIP remains one linear channel-major stream. It is never treated
    // as independently seekable compressed channel streams here.
    completed_rows = completed_rows
        .checked_add(verify_stream(source, &index.composite, scratch, check, pinned)?)
        .ok_or("Source36 completed row count overflow")?;
    completed_streams += 1;
    progress(VerificationPhase::Streams { total: streams }, completed_streams as u64)?;
    if completed_streams != streams
        || hash_range(source, whole, scratch, check, |bytes| {
            progress(VerificationPhase::AfterStreamsChecksum, bytes)
        })? != expected_source_sha256
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

fn verify_stream<R: Read + Seek>(
    source: &Source<R>,
    encoding: &Encoding,
    scratch: &mut [u8],
    check: &dyn Fn() -> Result<(), String>,
    pinned: &dyn Fn() -> Result<(), String>,
) -> Result<u64, String> {
    let row_bytes =
        usize::try_from(encoding.row_bytes).map_err(|_| "Source36 native row length overflow")?;
    if row_bytes > scratch.len() {
        return Err("Source36 native row exceeds admitted transfer scratch".into());
    }
    let mut rows = source.rows_with_checks(encoding, check, pinned)?;
    if encoding.compression == Compression::Raw {
        // Both fresh whole-file hashes cover this exact payload. Setting up
        // the row reader verifies its header and exact byte count. The later
        // fidelity pass reads and compares every sample at its original depth.
        check()?;
        return Ok(encoding.rows);
    }
    if row_bytes == 0 {
        for _ in 0..encoding.rows {
            if !rows.read_row(&mut [])? {
                return Err("Source36 native row stream ended early".into());
            }
        }
    } else {
        let capacity = (scratch.len() / row_bytes) as u64;
        let mut remaining = encoding.rows;
        while remaining > 0 {
            let count = remaining.min(capacity);
            rows.read_rows(&mut scratch[..count as usize * row_bytes])?;
            remaining -= count;
        }
    }
    rows.finish()?.check(source, encoding, check)?;
    Ok(encoding.rows)
}

#[cfg(test)]
#[test]
fn raw_stream_verification_checks_layout_without_reading_pixel_payload() {
    use super::structural::Format;

    // The outer whole-file hash reads the payload. Count just the stream
    // verifier's reads here to catch a redundant channel pass.
    let mut file = std::io::Cursor::new([0u8; 18]);
    let encoding = Encoding {
        format: Format::Psb,
        depth: 16,
        rows: 4,
        row_bytes: 4,
        row_padding: 0,
        compression: Compression::Raw,
        span: Span { offset: 0, bytes: 18 },
        counts: None,
        payload: Span { offset: 2, bytes: 16 },
    };
    let mut scratch = [0; 8];
    let check = || Ok(());
    {
        let source = Source::new(&mut file, 18).unwrap();
        assert_eq!(verify_stream(&source, &encoding, &mut scratch, &check, &check).unwrap(), 4);
    }
    assert_eq!(file.position(), 2);
    let mut wrong_rows = encoding;
    wrong_rows.rows -= 1;
    let source = Source::new(&mut file, 18).unwrap();
    assert!(verify_stream(&source, &wrong_rows, &mut scratch, &check, &check).is_err());
}

fn hash_range<R: Read + Seek>(
    source: &Source<R>,
    span: Span,
    scratch: &mut [u8],
    check: &dyn Fn() -> Result<(), String>,
    progress: impl FnMut(u64) -> Result<(), String>,
) -> Result<Digest, String> {
    let mut hash = Context::new(&SHA256);
    scan_range(source, span, scratch, check, progress, |_, bytes| {
        hash.update(bytes);
    })?;
    Ok(hash.finish().as_ref().try_into().unwrap())
}

fn hash_source_and_standard<R: Read + Seek>(
    source: &Source<R>,
    whole: Span,
    [layer, composite]: [Span; 2],
    scratch: &mut [u8],
    check: &dyn Fn() -> Result<(), String>,
    progress: impl FnMut(u64) -> Result<(), String>,
) -> Result<(Digest, Digest), String> {
    let layer_end = layer.end()?;
    let composite_end = composite.end()?;
    if layer.offset < whole.offset || layer_end > composite.offset || composite_end > whole.end()? {
        return Err("Source36 standard ranges do not fit their indexed source".into());
    }
    let mut whole_hash = Context::new(&SHA256);
    let mut standard_hash = bulk36::standard_hasher(layer.bytes, composite.bytes);
    let standard_ranges = [layer.offset..layer_end, composite.offset..composite_end];
    // Use the same authenticated observation for both bindings. The later
    // whole-file pass still verifies the bytes after native stream checks.
    scan_range(source, whole, scratch, check, progress, |offset, bytes| {
        whole_hash.update(bytes);
        let end = offset + bytes.len() as u64;
        for range in &standard_ranges {
            let first = offset.max(range.start);
            let last = end.min(range.end);
            if first < last {
                standard_hash.update(&bytes[(first - offset) as usize..(last - offset) as usize]);
            }
        }
    })?;
    Ok((
        whole_hash.finish().as_ref().try_into().unwrap(),
        standard_hash.finish().as_ref().try_into().unwrap(),
    ))
}

fn scan_range<R: Read + Seek>(
    source: &Source<R>,
    span: Span,
    scratch: &mut [u8],
    check: &dyn Fn() -> Result<(), String>,
    mut progress: impl FnMut(u64) -> Result<(), String>,
    mut observe: impl FnMut(u64, &[u8]),
) -> Result<(), String> {
    source.unchanged_length()?;
    let mut scanned = 0;
    progress(0)?;
    source.scan(span, scratch, check, |bytes| {
        observe(span.offset + scanned, bytes);
        scanned += bytes.len() as u64;
        if scanned < span.bytes {
            progress(scanned)?;
        }
        Ok(())
    })?;
    source.unchanged_length()?;
    if scanned != 0 {
        progress(scanned)?;
    }
    check()
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

#[cfg(test)]
#[test]
fn initial_scan_hashes_only_the_indexed_standard_spans_for_the_manifest() {
    let data: Vec<u8> = (0..24).collect();
    let whole = Span { offset: 0, bytes: 24 };
    let layer = Span { offset: 3, bytes: 8 };
    let composite = Span { offset: 13, bytes: 10 };
    let source = Source::new(std::io::Cursor::new(&data), whole.bytes).unwrap();
    let check = || Ok(());
    let mut reported = Vec::new();
    let (full_sha, standard_sha) =
        hash_source_and_standard(&source, whole, [layer, composite], &mut [0; 5], &check, |n| {
            reported.push(n);
            Ok(())
        })
        .unwrap();
    assert_eq!(reported, [0, 5, 10, 15, 20, 24]);
    assert_eq!(full_sha.as_slice(), ring::digest::digest(&SHA256, &data).as_ref());
    assert_eq!(
        standard_sha,
        bulk36::standard_digest(&mut &data[3..11], 8, &mut &data[13..23], 10, &check).unwrap()
    );
    for invalid in [Span { offset: 10, bytes: 4 }, Span { offset: 23, bytes: 2 }] {
        assert!(hash_source_and_standard(
            &source,
            whole,
            [layer, invalid],
            &mut [0; 5],
            &check,
            |_| Ok(())
        )
        .is_err());
    }
}

#[cfg(test)]
#[test]
fn whole_file_hash_reports_read_bytes_and_preserves_cancellation() {
    let data = [1u8; 9];
    let source = Source::new(std::io::Cursor::new(&data), data.len() as u64).unwrap();
    let whole = Span { offset: 0, bytes: 9 };
    let mut scratch = [0; 4];
    let mut reported = Vec::new();
    let mut report = |bytes| {
        reported.push(bytes);
        Ok(())
    };
    let sha = hash_range(&source, whole, &mut scratch, &|| Ok(()), &mut report).unwrap();
    assert_eq!(sha.as_slice(), ring::digest::digest(&SHA256, &data).as_ref());
    assert_eq!(reported, [0, 4, 8, 9]);

    reported.clear();
    let result = hash_range(&source, whole, &mut scratch, &|| Ok(()), |bytes| {
        reported.push(bytes);
        if bytes == 4 { Err("cancelled".into()) } else { Ok(()) }
    });
    assert_eq!(result.unwrap_err(), "cancelled");
    assert_eq!(reported, [0, 4]);
}

/// This token binds the checked raw layouts and completed compressed streams
/// to the whole-file digest. It does not attest to rendered pixel fidelity.
/// Exact source and index borrows prevent substituting a same-shaped object.
#[must_use = "Source observations require typed/render/lease/publication validation"]
pub(crate) struct CompletedSource<'source, 'index, R> {
    source: &'source Source<R>,
    index: &'index Index,
    whole: Digest,
    origin: (u64, Digest),
    bulk: CompletedBulk,
    standard: CompletedStandard,
    streams: usize,
    rows: u64,
    failed: Cell<bool>,
}
impl<R: Read + Seek> CompletedSource<'_, '_, R> {
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
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn recheck<T>(
        &self,
        source: &Source<R>,
        index: &Index,
        manifest: &Manifest<T>,
        expected_whole: Digest,
        scratch: &mut [u8],
        check: &dyn Fn() -> Result<(), String>,
        progress: impl FnMut(u64) -> Result<(), String>,
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
                progress,
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
        self.recheck(source, index, &manifest, expected_whole, scratch, check, |_| Ok(()))?;
        manifest.map_unique(|manifest, _| Ok(manifest.into_metadata()))
    }
}

/// A movable exact index owner for provisional import. It borrows the caller's
/// already admitted source handle and never grants file/path or edit authority.
pub(crate) struct OwnedSource<'a, R> {
    source: &'a Source<R>,
    index: Arc<Index>,
    whole: Digest,
    origin: (u64, Digest),
    bulk: CompletedBulk,
    standard: CompletedStandard,
    failed: Cell<bool>,
}
impl<'a, R: Read + Seek> CompletedSource<'a, '_, R> {
    pub(crate) fn own_index(self, index: Arc<Index>) -> Result<OwnedSource<'a, R>, String> {
        if self.failed.get() || !std::ptr::eq(self.index, index.as_ref()) {
            return Err("Source36 owned completion requires its exact observed index".into());
        }
        Ok(OwnedSource {
            source: self.source,
            index,
            whole: self.whole,
            origin: self.origin,
            bulk: self.bulk,
            standard: self.standard,
            failed: Cell::new(false),
        })
    }
}
impl<R: Read + Seek> OwnedSource<'_, R> {
    pub(crate) fn source(&self) -> &Source<R> {
        self.source
    }
    pub(crate) fn index(&self) -> &Index {
        &self.index
    }
    pub(crate) fn sha256(&self) -> Digest {
        self.whole
    }
    pub(crate) fn recheck(
        &self,
        binding: &super::manifest36::SourceBinding,
        scratch: &mut [u8],
        check: &dyn Fn() -> Result<(), String>,
        progress: impl FnMut(u64) -> Result<(), String>,
    ) -> Result<(), String> {
        if self.failed.replace(true) {
            return Err("Source36 owned source completion is failed".into());
        }
        let result = (|| {
            check()?;
            if binding.origin() != Some(self.origin)
                || scratch.is_empty()
                || scratch.len() > bulk36::MAX_TILE_BYTES
            {
                return Err("Source36 retained source binding/scratch differs".into());
            }
            self.bulk.check(binding.plan(), check)?;
            self.standard.check_binding(binding)?;
            self.source.unchanged_length()?;
            if hash_range(
                self.source,
                Span {
                    offset: 0,
                    bytes: self.index.file_bytes,
                },
                scratch,
                check,
                progress,
            )? != self.whole
            {
                return Err("Source36 admitted source changed before provisional handoff".into());
            }
            self.source.unchanged_length()?;
            check()
        })();
        if result.is_ok() {
            self.failed.set(false);
        }
        result
    }
}
