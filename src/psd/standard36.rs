//! Native standard channel emission. Prepared providers supply complete bounded
//! planar regions from one captured scene; sampling must not perform file I/O.
use super::{
    bulk36::{Descriptor, Digest, MAX_CHUNK_BYTES, MAX_TILE_BYTES},
    container36::HashWriter,
    structural::{Format, Header, Index, Span},
};
use std::io::{self, Read, Seek, Write};
pub(crate) const MAX_STANDARD_RECORDS: usize = 2 * 200 + 3;
pub(crate) const LAYER_INFO_MISMATCH: &str =
    "PeerBrush native Photoshop layer information or transparency marker differs from the saved layout";

pub(crate) fn check_layer_info<R: Read + Seek>(
    source: &super::row_stream::Source<R>,
    index: &Index,
    check: &dyn Fn() -> Result<(), String>,
) -> Result<(), String> {
    let active = index.active_layer_info;
    if index.header.depth == 16 {
        let mut layer_info = index.global_tags.iter().filter(|tag| tag.key == *b"Lr16");
        let mut transparency = index.global_tags.iter().filter(|tag| tag.key == *b"Mt16");
        if index.primary_layer_info.bytes != 0
            || !layer_info.next().is_some_and(|tag| {
                tag.signature == *b"8BIM" && tag.data == active
            })
            || layer_info.next().is_some()
            || !transparency.next().is_some_and(|tag| {
                tag.signature == *b"8BIM" && tag.data.bytes == 0
            })
            || transparency.next().is_some()
        {
            return Err(LAYER_INFO_MISMATCH.into());
        }
    } else if index.primary_layer_info != active {
        return Err(LAYER_INFO_MISMATCH.into());
    }
    if active.bytes < 2 || index.layers.is_empty() {
        return Err(LAYER_INFO_MISMATCH.into());
    }
    let mut count = [0; 2];
    source
        .range(Span { bytes: 2, ..active }, check)?
        .read_exact(&mut count)
        .map_err(|e| e.to_string())?;
    let count = i16::from_be_bytes(count);
    if count >= 0 || usize::from(count.unsigned_abs()) != index.layers.len() {
        return Err(LAYER_INFO_MISMATCH.into());
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RowSource {
    Layer(usize),
    /// Original source bytes, without effect rendering or adjustment
    /// replacement. Native hidden RGB and channel low bits remain significant.
    OriginalLayer(usize),
    MaskBeforeDensity { layer: usize, originals: bool },
    BakedComposite,
    MergedWhiteMatte,
    Zero,
}
#[derive(Clone, Copy, Debug)]
pub(crate) struct RowRequest {
    pub provenance: Digest,
    pub source: RowSource,
    pub channel: i16,
    /// Document-global [left,top,right,bottom]; first_row is relative to top.
    pub bounds: [i32; 4],
    pub first_row: u32,
    pub rows: u32,
    pub depth: u16,
}
pub(crate) trait NativeRows {
    fn provenance(&self) -> Digest;
    /// The writer and readback may each prepare one independent planar pass.
    fn prepare_merged_pass(&mut self, _check: &dyn Fn() -> Result<(), String>) -> Result<(), String> {
        Ok(())
    }
    fn finish_merged_pass(&mut self) {}
    /// Fill exactly rows*width*(depth/8) bytes in channel-major native BE order.
    /// For -2 prepare actual mask steps even when the mask is disabled. Its
    /// disabled flag lives in the record, not in replacement all-unit samples.
    fn fill(
        &mut self,
        request: RowRequest,
        output: &mut [u8],
        check: &dyn Fn() -> Result<(), String>,
    ) -> Result<(), String>;
}
pub(crate) struct Metadata<'a> {
    pub key: [u8; 4],
    pub bytes: &'a [u8],
}
#[derive(Clone, Copy)]
pub(crate) struct Mask {
    pub bounds: [i32; 4],
    pub enabled: bool,
    pub linked: bool,
    pub default: u8,
    pub dual: bool,
    pub density: Option<u8>,
}
pub(crate) struct Record<'a> {
    pub source: RowSource,
    pub bounds: [i32; 4],
    pub mask: Option<Mask>,
    pub name: &'a str,
    pub blend: [u8; 4],
    pub opacity: u8,
    pub clipped: bool,
    pub visible: bool,
    pub locked: bool,
    pub invert: bool,
    pub tone: Option<super::adjustments::ToneRecord>,
    pub solid: Option<crate::raster::Pixel>,
    pub standard_style: Option<super::styles::StandardStyles<'a>>,
    pub gradient: Option<&'a serde_json::Value>,
    pub pattern: Option<super::patterns::FillRecord>,
    pub has_clipped_layers: bool,
    pub section: u32,
    pub metadata: Vec<Metadata<'a>>,
}

/// Record and metadata vector capacities must already be admitted by the caller.
/// They borrow the immutable typed snapshot; no native samples are serialized.
pub(crate) struct Plan<'a> {
    header: Header,
    provenance: Digest,
    records: Vec<Record<'a>>,
    global_mask: &'a [u8],
    global_metadata: &'a [super::GlobalMetadata],
    patterns: super::patterns::StandardPatterns,
    chunks: &'a [u32],
    info_raw: u64,
    info_bytes: u64,
    prefix_bytes: u64,
    layer_bytes: u64,
    layer_rows: usize,
    composite_bytes: u64,
}
impl<'a> Plan<'a> {
    pub(crate) fn new(
        header: Header,
        provenance: Digest,
        records: Vec<Record<'a>>,
        global_mask: &'a [u8],
        global_metadata: &'a [super::GlobalMetadata],
        patterns: super::patterns::StandardPatterns,
        chunks: &'a [u32],
        check: &dyn Fn() -> Result<(), String>,
    ) -> Result<Self, String> {
        check()?;
        if header.channels != 4
            || ![8, 16].contains(&header.depth)
            || header.width == 0
            || header.height == 0
            || header.width > 16000
            || header.height > 16000
            || records.is_empty()
            || records.len() > MAX_STANDARD_RECORDS
            || chunks.len() > 16384
            || global_mask.len() > 16
        {
            return Err(
                "Source36 standard plan exceeds its declared native/record envelope".into(),
            );
        }
        super::global_metadata::validate(global_metadata)?;
        let mut info_raw = 2u64;
        let mut layer_rows = 0;
        for record in &records {
            validate(record)?;
            let mut counter = HashWriter::new(io::sink(), 128 * 1024 * 1024, check);
            write_record(&mut counter, record, header)?;
            let (bytes, _) = counter.finish();
            info_raw = info_raw
                .checked_add(bytes)
                .ok_or("Source36 layer record overflow")?;
            for (_, bounds) in channels(record) {
                info_raw = info_raw
                    .checked_add(encoded_bytes(bounds, header.depth)?)
                    .ok_or("Source36 layer channel size overflow")?;
                if bounds[2] > bounds[0] {
                    layer_rows += (bounds[3] - bounds[1]) as usize;
                }
            }
        }
        let info_bytes = info_raw
            .checked_add(info_raw % 2)
            .ok_or("Source36 layer information overflow")?;
        let length_width = if header.format == Format::Psb { 8 } else { 4 };
        if header.format == Format::Psd && info_bytes > u64::from(u32::MAX) {
            return Err("Source36 layer information requires PSB's 64-bit length".into());
        }
        let prefix_bytes = if header.depth == 16 {
            length_width
                + 4
                + global_mask.len() as u64
                + 8
                + length_width
                + info_bytes
                + (4 - info_bytes % 4) % 4
                + 8
                + length_width
        } else {
            length_width + info_bytes + 4 + global_mask.len() as u64
        };
        let prefix_bytes = prefix_bytes
            + super::global_metadata::encoded_bytes(global_metadata, layout(header.format))
            + patterns.encoded_bytes();
        let mut layer_bytes = prefix_bytes;
        for &bytes in chunks {
            check()?;
            if !(56..=MAX_CHUNK_BYTES).contains(&bytes) {
                return Err("Source36 bulk tag payload exceeds its ordinary32-bit envelope".into());
            }
            layer_bytes = layer_bytes
                .checked_add(12 + u64::from(bytes) + (4 - u64::from(bytes) % 4) % 4)
                .ok_or("Source36 bulk tag section overflow")?;
        }
        let composite_bytes = u64::from(header.width)
            .checked_mul(u64::from(header.height))
            .and_then(|n| n.checked_mul(u64::from(header.depth / 8) * 4))
            .and_then(|n| n.checked_add(2))
            .ok_or("Source36 merged composite size overflow")?;
        // Includes only the minimal fixed container/resources; the exact final
        // LayoutPlan rechecks the complete compressed manifest before replay.
        if header.format == Format::Psd
            && layer_bytes
                .checked_add(composite_bytes)
                .and_then(|n| n.checked_add(78))
                .is_none_or(|n| n > 2_000_000_000)
        {
            return Err("Source36 standard data exceeds PSD's 2 GB limit; choose PSB".into());
        }
        if layer_bytes
            .checked_add(composite_bytes)
            .is_none_or(|n| n > 64 * 1024 * 1024 * 1024)
        {
            return Err("Source36 standard data exceeds its 64 GiB operation envelope".into());
        }
        Ok(Self {
            header,
            provenance,
            records,
            global_mask,
            global_metadata,
            patterns,
            chunks,
            info_raw,
            info_bytes,
            prefix_bytes,
            layer_bytes,
            layer_rows,
            composite_bytes,
        })
    }
    pub(crate) fn header(&self) -> Header {
        self.header
    }
    pub(crate) fn provenance(&self) -> Digest {
        self.provenance
    }
    pub(crate) fn records(&self) -> &[Record<'a>] {
        &self.records
    }
    pub(crate) fn layer_bytes(&self) -> u64 {
        self.layer_bytes
    }
    pub(crate) fn composite_bytes(&self) -> u64 {
        self.composite_bytes
    }
    pub(crate) fn chunk_span(&self, ordinal: usize) -> Result<super::structural::Span, String> {
        let bytes = *self
            .chunks
            .get(ordinal)
            .ok_or("Missing Source36 bulk chunk ordinal")?;
        let offset =
            self.chunks[..ordinal]
                .iter()
                .try_fold(self.prefix_bytes, |offset, &bytes| {
                    offset
                        .checked_add(12 + u64::from(bytes) + (4 - u64::from(bytes) % 4) % 4)
                        .ok_or("Source36 bulk chunk position overflow")
                })?;
        Ok(super::structural::Span {
            offset: offset + 12,
            bytes: u64::from(bytes),
        })
    }

    #[cfg(test)]
    pub(crate) fn write_layers<W: Write + ?Sized>(
        &self,
        output: &mut W,
        provider: &mut impl NativeRows,
        scratch: &mut [u8],
        check: &dyn Fn() -> Result<(), String>,
        progress: impl FnMut(usize, usize) -> Result<(), String>,
        bulk: impl FnMut(usize, &mut dyn Write) -> Result<Descriptor, String>,
    ) -> Result<WrittenLayers, String> {
        self.write_layers_with_checks(output, provider, scratch, (check, check), progress, bulk)
    }

    /// Append exact standard records/channels and verify private tag payloads against actual writes.
    pub(crate) fn write_layers_with_checks<W: Write + ?Sized>(
        &self,
        output: &mut W,
        provider: &mut impl NativeRows,
        scratch: &mut [u8],
        (check, pinned): (&dyn Fn() -> Result<(), String>, &dyn Fn() -> Result<(), String>),
        progress: impl FnMut(usize, usize) -> Result<(), String>,
        mut bulk: impl FnMut(usize, &mut dyn Write) -> Result<Descriptor, String>,
    ) -> Result<WrittenLayers, String> {
        self.check_provider(provider, scratch, check)?;
        let mut report_rows = row_progress(self.layer_rows, progress)?;
        // The actual spool checks ownership before and after its writes.
        let mut output = HashWriter::new(output, self.layer_bytes, pinned);
        if self.header.depth == 16 {
            length(&mut output, 0, self.header.format)?;
            bytes32(&mut output, self.global_mask)?;
            output.write_all(b"8BIMMt16").map_err(|e| e.to_string())?;
            length(&mut output, 0, self.header.format)?;
            output.write_all(b"8BIMLr16").map_err(|e| e.to_string())?;
        }
        length(&mut output, self.info_bytes, self.header.format)?;
        output
            .write_all(&(-(self.records.len() as i16)).to_be_bytes())
            .map_err(|e| e.to_string())?;
        for record in &self.records {
            write_record(&mut output, record, self.header)?;
        }
        for record in &self.records {
            for (channel, bounds) in channels(record) {
                output
                    .write_all(&0u16.to_be_bytes())
                    .map_err(|e| e.to_string())?;
                self.rows(
                    &mut output,
                    provider,
                    channel_source(record, channel),
                    if channel == -3 { -2 } else { channel },
                    bounds,
                    scratch,
                    check,
                    &mut report_rows,
                )?;
            }
        }
        pad(&mut output, self.info_raw, 2)?;
        if self.header.depth == 16 {
            pad(&mut output, self.info_bytes, 4)?;
        } else {
            bytes32(&mut output, self.global_mask)?;
        }
        for metadata in self.global_metadata {
            check()?;
            super::global_metadata::write(&mut output, metadata, layout(self.header.format))?;
        }
        self.patterns.write(&mut output)?;
        let mut descriptors = Vec::new();
        descriptors
            .try_reserve_exact(self.chunks.len())
            .map_err(|_| "Source36 chunk descriptor allocation refused")?;
        for (ordinal, &expected) in self.chunks.iter().enumerate() {
            check()?;
            output.write_all(b"8BIMPb36").map_err(|e| e.to_string())?;
            output
                .write_all(&expected.to_be_bytes())
                .map_err(|e| e.to_string())?;
            let mut chunk = HashWriter::new(&mut output, u64::from(expected), pinned);
            let descriptor = bulk(ordinal, &mut chunk)?;
            let (bytes, sha256) = chunk.finish();
            if bytes != u64::from(expected)
                || descriptor.bytes != expected
                || descriptor.ordinal as usize != ordinal
                || descriptor.sha256 != sha256
            {
                return Err(
                    "Source36 private chunk callback differs from its exact staged bytes".into(),
                );
            }
            descriptors.push(descriptor);
            pad(&mut output, u64::from(expected), 4)?;
        }
        output.flush().map_err(|e| e.to_string())?;
        check()?;
        let (bytes, sha256) = output.finish();
        if bytes != self.layer_bytes {
            return Err("Source36 standard layer replay length differs".into());
        }
        Ok(WrittenLayers {
            bytes,
            sha256,
            descriptors,
        })
    }
    pub(crate) fn write_composite<W: Write + ?Sized>(
        &self,
        output: &mut W,
        provider: &mut impl NativeRows,
        scratch: &mut [u8],
        check: &dyn Fn() -> Result<(), String>,
        progress: impl FnMut(usize, usize) -> Result<(), String>,
    ) -> Result<WrittenComposite, String> {
        self.check_provider(provider, scratch, check)?;
        let mut output = HashWriter::new(output, self.composite_bytes, check);
        output
            .write_all(&0u16.to_be_bytes())
            .map_err(|e| e.to_string())?;
        let bounds = [0, 0, self.header.width as i32, self.header.height as i32];
        provider.prepare_merged_pass(check)?;
        let channels = (|| {
            let mut report_rows = row_progress(self.header.height as usize * 4, progress)?;
            (0..4).try_for_each(|channel| {
                self.rows(
                    &mut output,
                    provider,
                    RowSource::MergedWhiteMatte,
                    channel,
                    bounds,
                    scratch,
                    check,
                    &mut report_rows,
                )
            })
        })();
        provider.finish_merged_pass();
        channels?;
        output.flush().map_err(|e| e.to_string())?;
        check()?;
        let (bytes, sha256) = output.finish();
        if bytes != self.composite_bytes {
            return Err("Source36 standard composite length differs".into());
        }
        Ok(WrittenComposite { bytes, sha256 })
    }
    /// Compare the reopened standard records and every native sample against
    /// freshly prepared rows from the same captured scene. A valid zlib stream
    /// or matching whole-file checksum alone cannot substitute for this check.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn compare<R: Read + Seek>(
        &self,
        source: &super::row_stream::Source<R>,
        index: &Index,
        provider: &mut impl NativeRows,
        expected: &mut [u8],
        actual: &mut [u8],
        check: &dyn Fn() -> Result<(), String>,
        progress: impl FnMut(usize, usize) -> Result<(), String>,
    ) -> Result<ComparedStandard, String> {
        self.check_provider(provider, expected, check)?;
        if index.header != self.header
            || index.layers.len() != self.records.len()
            || actual.is_empty()
            || actual.len() > MAX_TILE_BYTES
        {
            return Err("Source36 reopened standard header/record/scratch differs".into());
        }
        source.unchanged_length()?;
        if index.active_layer_info.bytes != self.info_bytes {
            return Err(LAYER_INFO_MISMATCH.into());
        }
        check_layer_info(source, index, check)?;
        let mut report_rows = row_progress(self.layer_rows + self.header.height as usize * 4, progress)?;
        let mut samples = 0u64;
        for (record, parsed) in self.records.iter().zip(&index.layers) {
            check()?;
            let mut hash = HashWriter::new(io::sink(), 128 * 1024 * 1024, check);
            write_record(&mut hash, record, self.header)?;
            let (bytes, digest) = hash.finish();
            if bytes != parsed.record.bytes
                || hash_span(source, parsed.record, actual, check)? != digest
            {
                return Err(
                    "Source36 reopened standard layer metadata differs from captured plan".into(),
                );
            }
            let mut native = parsed.channels.iter();
            for (channel, bounds) in channels(record) {
                let parsed = native
                    .next()
                    .ok_or("Missing reopened native layer/mask channel")?;
                if parsed.id != channel || parsed.encoding_error.is_some() {
                    return Err("Source36 reopened standard channel identity differs".into());
                }
                let encoding = parsed
                    .encoding
                    .as_ref()
                    .ok_or("Unvalidated reopened native layer/mask channel")?;
                let mut rows = source.rows(encoding, check)?;
                samples = samples
                    .checked_add(self.compare_rows(
                        &mut rows,
                        provider,
                        channel_source(record, channel),
                        if channel == -3 { -2 } else { channel },
                        bounds,
                        expected,
                        actual,
                        check,
                        &mut report_rows,
                    )?)
                    .ok_or("Source36 native comparison sample count overflow")?;
                rows.finish()?.check(source, encoding, check)?;
            }
            if native.next().is_some() {
                return Err("Excess reopened native layer/mask channels".into());
            }
        }
        let global_digest: Digest = ring::digest::digest(&ring::digest::SHA256, self.global_mask)
            .as_ref()
            .try_into()
            .unwrap();
        if index.global_mask.bytes != self.global_mask.len() as u64
            || !self.global_mask.is_empty()
                && hash_span(source, index.global_mask, actual, check)? != global_digest
        {
            return Err("Source36 reopened global mask differs from captured metadata".into());
        }
        let mut retained = self.global_metadata.iter();
        let mut saw_patterns = false;
        for tag in &index.global_tags {
            if tag.key == *b"Pb36"
                || self.header.depth == 16 && matches!(&tag.key, b"Lr16" | b"Mt16")
            {
                continue;
            }
            if tag.key == *b"Patt" && !self.patterns.bytes().is_empty() {
                let expected = ring::digest::digest(&ring::digest::SHA256, self.patterns.bytes());
                if saw_patterns || tag.signature != *b"8BIM" || tag.data.bytes != self.patterns.bytes().len() as u64
                    || hash_span(source, tag.data, actual, check)?.as_slice() != expected.as_ref()
                {
                    return Err("Source36 embedded Photoshop patterns differ from captured sources".into());
                }
                saw_patterns = true;
                continue;
            }
            let expected = retained
                .next()
                .ok_or("Unexpected Source36 document metadata")?;
            let digest: Digest = ring::digest::digest(&ring::digest::SHA256, &expected.data)
                .as_ref()
                .try_into()
                .unwrap();
            if tag.signature != *b"8BIM"
                || tag.key.as_slice() != expected.key.as_bytes()
                || tag.data.bytes != expected.data.len() as u64
                || hash_span(source, tag.data, actual, check)? != digest
            {
                return Err("Source36 document metadata differs from captured sources".into());
            }
        }
        if retained.next().is_some() || saw_patterns != !self.patterns.bytes().is_empty() {
            return Err("Missing Source36 document metadata".into());
        }
        let mut merged = source.rows(&index.composite, check)?;
        let bounds = [0, 0, self.header.width as i32, self.header.height as i32];
        provider.prepare_merged_pass(check)?;
        let channels = (0..4).try_for_each(|channel| {
            samples = samples
                .checked_add(self.compare_rows(
                    &mut merged,
                    provider,
                    RowSource::MergedWhiteMatte,
                    channel,
                    bounds,
                    expected,
                    actual,
                    check,
                    &mut report_rows,
                )?)
                .ok_or("Source36 native comparison sample count overflow")?;
            Ok::<_, String>(())
        });
        provider.finish_merged_pass();
        channels?;
        merged.finish()?.check(source, &index.composite, check)?;
        source.unchanged_length()?;
        self.check_provider(provider, expected, check)?;
        Ok(ComparedStandard {
            plan: self.fingerprint(check)?,
            samples,
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn compare_rows<R: Read + Seek>(
        &self,
        rows: &mut super::row_stream::Rows<'_, R>,
        provider: &mut impl NativeRows,
        source: RowSource,
        channel: i16,
        bounds: [i32; 4],
        expected: &mut [u8],
        actual: &mut [u8],
        check: &dyn Fn() -> Result<(), String>,
        report_rows: &mut impl FnMut(u32) -> Result<(), String>,
    ) -> Result<u64, String> {
        let (width, height) = dimensions(bounds)?;
        if width == 0 || height == 0 {
            return Ok(0);
        }
        let row_bytes = width as usize * usize::from(self.header.depth / 8);
        if actual.len() < row_bytes {
            return Err("Source36 native comparison row exceeds admitted scratch".into());
        }
        let transfer = actual.len() / row_bytes * row_bytes;
        let stripe = stripe_rows(expected.len(), width, self.header.depth);
        if stripe == 0 {
            return Err("Source36 expected native stripe exceeds admitted scratch".into());
        }
        let mut first_row = 0;
        while first_row < height {
            check()?;
            let count = stripe.min(height - first_row);
            let wanted = &mut expected[..count as usize * row_bytes];
            if source == RowSource::Zero {
                wanted.fill(0);
            } else {
                provider.fill(
                    RowRequest {
                        provenance: self.provenance,
                        source,
                        channel,
                        bounds,
                        first_row,
                        rows: count,
                        depth: self.header.depth,
                    },
                    wanted,
                    check,
                )?;
            }
            check()?;
            if provider.provenance() != self.provenance {
                return Err(
                    "Source36 native comparison provider changed its captured scene".into(),
                );
            }
            for batch in wanted.chunks(transfer) {
                let actual = &mut actual[..batch.len()];
                rows.read_rows(actual)?;
                if &*actual != batch {
                    return Err("Source36 reopened native samples differ from current captured render/source".into());
                }
            }
            first_row += count;
            report_rows(count)?;
        }
        Ok(u64::from(width) * u64::from(height))
    }
    fn fingerprint(&self, check: &dyn Fn() -> Result<(), String>) -> Result<Digest, String> {
        let mut hash = ring::digest::Context::new(&ring::digest::SHA256);
        hash.update(b"PeerBrushSource36StandardPlan\0");
        hash.update(&self.provenance);
        hash.update(&[if self.header.format == Format::Psb {
            2
        } else {
            1
        }]);
        hash.update(&self.header.width.to_be_bytes());
        hash.update(&self.header.height.to_be_bytes());
        hash.update(&self.header.depth.to_be_bytes());
        hash.update(&(self.records.len() as u64).to_be_bytes());
        for record in &self.records {
            check()?;
            let (kind, index) = match record.source {
                RowSource::Layer(i) => (0, i),
                RowSource::OriginalLayer(i) => (1, i),
                RowSource::BakedComposite => (2, 0),
                RowSource::MergedWhiteMatte => (3, 0),
                RowSource::Zero => (4, 0),
                RowSource::MaskBeforeDensity { layer, originals } => (5 + u8::from(originals), layer),
            };
            hash.update(&[kind]);
            hash.update(&(index as u64).to_be_bytes());
            let mut output = HashWriter::new(io::sink(), 128 * 1024 * 1024, check);
            write_record(&mut output, record, self.header)?;
            let (bytes, sha) = output.finish();
            hash.update(&bytes.to_be_bytes());
            hash.update(&sha);
        }
        hash.update(&(self.global_mask.len() as u64).to_be_bytes());
        hash.update(self.global_mask);
        hash.update(&(self.global_metadata.len() as u64).to_be_bytes());
        for entry in self.global_metadata {
            hash.update(entry.key.as_bytes());
            hash.update(&(entry.data.len() as u64).to_be_bytes());
            hash.update(&entry.data);
        }
        if !self.patterns.bytes().is_empty() {
            hash.update(b"Patt");
            hash.update(&(self.patterns.bytes().len() as u64).to_be_bytes());
            hash.update(self.patterns.bytes());
        }
        hash.update(&(self.chunks.len() as u64).to_be_bytes());
        for chunk in self.chunks {
            hash.update(&chunk.to_be_bytes());
        }
        check()?;
        Ok(hash.finish().as_ref().try_into().unwrap())
    }
    fn check_provider(
        &self,
        provider: &impl NativeRows,
        scratch: &[u8],
        check: &dyn Fn() -> Result<(), String>,
    ) -> Result<(), String> {
        check()?;
        if provider.provenance() != self.provenance
            || scratch.is_empty()
            || scratch.len() > MAX_TILE_BYTES
        {
            return Err(
                "Source36 native row provider or scratch differs from captured admission".into(),
            );
        }
        Ok(())
    }
    fn rows(
        &self,
        output: &mut impl Write,
        provider: &mut impl NativeRows,
        source: RowSource,
        channel: i16,
        bounds: [i32; 4],
        scratch: &mut [u8],
        check: &dyn Fn() -> Result<(), String>,
        report_rows: &mut impl FnMut(u32) -> Result<(), String>,
    ) -> Result<(), String> {
        let (w, h) = dimensions(bounds)?;
        if w == 0 || h == 0 {
            return Ok(());
        }
        let row_bytes = w as usize * usize::from(self.header.depth / 8);
        let stripe = stripe_rows(scratch.len(), w, self.header.depth);
        if stripe == 0 {
            return Err("Source36 native row exceeds admitted stripe transfer".into());
        }
        let mut first_row = 0;
        while first_row < h {
            check()?;
            let rows = stripe.min(h - first_row);
            let out = &mut scratch[..rows as usize * row_bytes];
            if source == RowSource::Zero {
                out.fill(0);
            } else {
                if provider.provenance() != self.provenance {
                    return Err("Source36 native provider changed its captured scene".into());
                }
                provider.fill(
                    RowRequest {
                        provenance: self.provenance,
                        source,
                        channel,
                        bounds,
                        first_row,
                        rows,
                        depth: self.header.depth,
                    },
                    out,
                    check,
                )?;
            }
            check()?;
            if provider.provenance() != self.provenance {
                return Err("Source36 native provider changed its captured scene".into());
            }
            output.write_all(out).map_err(|e| e.to_string())?;
            first_row += rows;
            report_rows(rows)?;
        }
        Ok(())
    }
}

fn row_progress(
    total: usize,
    mut progress: impl FnMut(usize, usize) -> Result<(), String>,
) -> Result<impl FnMut(u32) -> Result<(), String>, String> {
    progress(0, total)?;
    let mut completed = 0;
    Ok(move |rows| {
        completed += rows as usize;
        progress(completed, total)
    })
}

pub(crate) struct WrittenLayers {
    pub bytes: u64,
    pub sha256: Digest,
    pub descriptors: Vec<Descriptor>,
}
pub(crate) struct WrittenComposite {
    pub bytes: u64,
    pub sha256: Digest,
}
#[must_use = "Standard fidelity still requires complete source/ownership/publication proof"]
pub(crate) struct ComparedStandard {
    plan: Digest,
    samples: u64,
}
impl ComparedStandard {
    pub(crate) fn check(
        &self,
        plan: &Plan<'_>,
        check: &dyn Fn() -> Result<(), String>,
    ) -> Result<(), String> {
        if self.plan != plan.fingerprint(check)? {
            return Err("Source36 native fidelity belongs to another captured plan".into());
        }
        Ok(())
    }
    pub(crate) fn samples(&self) -> u64 {
        self.samples
    }
}
fn hash_span<R: Read + Seek>(
    source: &super::row_stream::Source<R>,
    span: super::structural::Span,
    scratch: &mut [u8],
    check: &dyn Fn() -> Result<(), String>,
) -> Result<Digest, String> {
    let mut input = source.range(span, check)?;
    let mut left = span.bytes;
    let mut hash = ring::digest::Context::new(&ring::digest::SHA256);
    while left != 0 {
        check()?;
        let n = left.min(scratch.len().min(65536) as u64) as usize;
        input
            .read_exact(&mut scratch[..n])
            .map_err(|e| e.to_string())?;
        hash.update(&scratch[..n]);
        left -= n as u64;
    }
    Ok(hash.finish().as_ref().try_into().unwrap())
}

fn dimensions(bounds: [i32; 4]) -> Result<(u32, u32), String> {
    let [l, t, r, b] = bounds;
    let w = i64::from(r) - i64::from(l);
    let h = i64::from(b) - i64::from(t);
    if [l, t].iter().any(|v| v.unsigned_abs() > 100000)
        || !(0..=16000).contains(&w)
        || !(0..=16000).contains(&h)
    {
        return Err("Source36 standard bounds exceed native source geometry".into());
    }
    Ok((w as u32, h as u32))
}
fn encoded_bytes(bounds: [i32; 4], depth: u16) -> Result<u64, String> {
    let (w, h) = dimensions(bounds)?;
    u64::from(w)
        .checked_mul(u64::from(h))
        .and_then(|n| n.checked_mul(u64::from(depth / 8)))
        .and_then(|n| n.checked_add(2))
        .ok_or_else(|| "Source36 raw channel length overflow".into())
}
fn layout(format: Format) -> super::Layout {
    match format {
        Format::Psd => super::Layout::Psd,
        Format::Psb => super::Layout::Psb,
    }
}
fn channels(record: &Record<'_>) -> impl Iterator<Item = (i16, [i32; 4])> {
    let bounds = record.bounds;
    let mask = record.mask;
    [-1, 0, 1, 2]
        .into_iter()
        .map(move |c| (c, bounds))
        .chain(mask.map(|mask| (-2, mask.bounds)))
        .chain(mask.filter(|mask| mask.dual).map(|mask| (-3, mask.bounds)))
}
fn channel_source(record: &Record<'_>, channel: i16) -> RowSource {
    if channel == -2 && record.mask.is_some_and(|mask| mask.density.is_some()) {
        match record.source {
            RowSource::Layer(layer) => RowSource::MaskBeforeDensity { layer, originals: false },
            RowSource::OriginalLayer(layer) => RowSource::MaskBeforeDensity { layer, originals: true },
            source => source,
        }
    } else {
        record.source
    }
}
fn validate(record: &Record<'_>) -> Result<(), String> {
    dimensions(record.bounds)?;
    if let Some(mask) = record.mask {
        dimensions(mask.bounds)?;
    }
    if record.section > 3
        || record.section != 0 && record.bounds != [0; 4]
        || record.section == 3 && record.mask.is_some()
        || record.mask.is_some_and(|mask| mask.density.is_some()
            && (mask.dual || !matches!(record.source, RowSource::Layer(_) | RowSource::OriginalLayer(_))))
        || matches!(record.source, RowSource::MaskBeforeDensity { .. })
        || (record.invert || record.tone.is_some()) && (record.section != 0 || record.blend == *b"pass"
            || super::blend_name(&record.blend).is_none())
        || record.tone.as_ref().is_some_and(|tone| record.invert || record.solid.is_some()
            || record.gradient.is_some() || record.pattern.is_some() || !tone.valid())
        || record.solid.is_some_and(|color| record.section != 0 || record.invert || color[3] != 255)
        || record.standard_style.is_some_and(|overlay| record.solid.is_some() != overlay.fill || record.bounds != overlay.bounds
            || record.mask.is_some_and(|mask| mask.dual) || record.mask.map(|mask| mask.linked) != overlay.mask_linked
            || record.blend != overlay.layer_blend || record.opacity != overlay.layer_opacity
            || record.clipped || record.has_clipped_layers || !matches!(record.source, RowSource::OriginalLayer(_)))
        || record.gradient.is_some_and(|settings| record.section != 0 || record.invert || record.solid.is_some()
            || record.blend == *b"pass" || !super::gradient_fill::representable(settings))
        || record.pattern.is_some() && (record.section != 0 || record.invert || record.solid.is_some()
            || record.gradient.is_some() || record.blend != *b"norm")
        || record.metadata.len() > super::MAX_LAYER_METADATA
        || record.section == 3 && record.metadata.iter().any(|entry| {
            !std::str::from_utf8(&entry.key).is_ok_and(super::closing_metadata_key)
        })
        || record.name.len() > super::canonical36::MAX_BYTES
        || record.metadata.iter().any(|m| m.bytes.len() > 128)
    {
        return Err("Source36 standard layer metadata exceeds its admitted shape".into());
    }
    Ok(())
}
fn write_record(
    output: &mut impl Write,
    record: &Record<'_>,
    header: Header,
) -> Result<(), String> {
    let [l, t, r, b] = record.bounds;
    for value in [t, l, b, r] {
        output
            .write_all(&value.to_be_bytes())
            .map_err(|e| e.to_string())?;
    }
    output
        .write_all(&(channels(record).count() as u16).to_be_bytes())
        .map_err(|e| e.to_string())?;
    for (channel, bounds) in channels(record) {
        output
            .write_all(&channel.to_be_bytes())
            .map_err(|e| e.to_string())?;
        length(output, encoded_bytes(bounds, header.depth)?, header.format)?;
    }
    output.write_all(b"8BIM").map_err(|e| e.to_string())?;
    output.write_all(&record.blend).map_err(|e| e.to_string())?;
    output
        .write_all(&[
            record.opacity,
            u8::from(record.clipped),
            u8::from(record.locked && record.section != 3)
                | if record.visible { 0 } else { 2 }
                | if record.invert || record.tone.is_some() || record.solid.is_some() || record.gradient.is_some() || record.pattern.is_some() { 0x18 } else { 0 },
            0,
        ])
        .map_err(|e| e.to_string())?;
    let mut count = HashWriter::new(io::sink(), 128 * 1024 * 1024, &|| Ok(()));
    write_extra(&mut count, record)?;
    let (bytes, _) = count.finish();
    output
        .write_all(
            &(u32::try_from(bytes).map_err(|_| "Source36 extra record overflow")?).to_be_bytes(),
        )
        .map_err(|e| e.to_string())?;
    write_extra(output, record)
}
fn write_extra(output: &mut impl Write, record: &Record<'_>) -> Result<(), String> {
    if let Some(mask) = record.mask {
        let payload = super::standard_mask_payload(
            mask.bounds, mask.default, super::standard_mask_flags(mask.enabled, mask.linked), mask.dual, mask.density,
        );
        bytes32(output, &payload[..if mask.dual { 36 } else { 20 }])?;
    } else {
        output
            .write_all(&0u32.to_be_bytes())
            .map_err(|e| e.to_string())?;
    }
    let ranges = record.metadata.iter()
        .find(|metadata| metadata.key == super::blend_ranges::TAG)
        .map(|metadata| super::blend_ranges::pairs(metadata.bytes));
    bytes32(output, ranges.as_ref().map_or(&[][..], |pairs| pairs.as_flattened()))?;
    let name = if record.section == 3 {
        "</Layer group>"
    } else {
        record.name
    };
    let legacy_name = super::legacy_layer_name(name);
    output.write_all(&[legacy_name.len() as u8]).map_err(|e| e.to_string())?;
    output.write_all(legacy_name).map_err(|e| e.to_string())?;
    pad(output, 1 + legacy_name.len() as u64, 4)?;
    let units = super::layer_name_units(name)?;
    tag_start(output, *b"luni", 4 + u64::from(units) * 2)?;
    output
        .write_all(&units.to_be_bytes())
        .map_err(|e| e.to_string())?;
    let mut transfer = [0; 1024];
    let mut used = 0;
    for unit in name.encode_utf16() {
        transfer[used..used + 2].copy_from_slice(&unit.to_be_bytes());
        used += 2;
        if used == transfer.len() {
            output.write_all(&transfer).map_err(|e| e.to_string())?;
            used = 0;
        }
    }
    output
        .write_all(&transfer[..used])
        .map_err(|e| e.to_string())?;
    if record.mask.is_some_and(|mask| !mask.linked) {
        tag(output, *b"lmgm", &[0; 4])?;
    }
    if record.locked && record.section != 3 && !record.metadata.iter().any(|entry| entry.key == *b"lspf") {
        tag(output, *b"lspf", &7u32.to_be_bytes())?;
    }
    if record.invert {
        tag(output, *b"nvrt", &[])?;
    }
    if let Some(tone) = &record.tone {
        if let Some((key, bytes)) = tone.companion() {
            tag(output, key, bytes)?;
        }
        let (key, bytes) = tone.parts();
        tag(output, key, bytes)?;
    }
    if let Some(color) = record.solid {
        tag(output, *b"SoCo", &super::adjustments::solid_descriptor(color))?;
    }
    if let Some(overlay) = &record.standard_style {
        super::styles::write_standard_style_tag(output, overlay).map_err(|error| error.to_string())?;
    }
    if let Some(settings) = record.gradient {
        tag(output, *b"GdFl", super::gradient_fill::descriptor(settings)?.bytes())?;
    }
    if let Some(id) = record.pattern {
        tag(output, *b"PtFl", super::patterns::descriptor(id).bytes())?;
    }
    if record.has_clipped_layers && record.section != 3
        && !record.metadata.iter().any(|metadata| metadata.key == *b"clbl")
    {
        tag(output, *b"clbl", &[1, 0, 0, 0])?;
    }
    if record.section != 0 {
        if record.section != 3 && record.blend == *b"pass" {
            let mut bytes = [0; 12];
            bytes[..4].copy_from_slice(&record.section.to_be_bytes());
            bytes[4..].copy_from_slice(b"8BIMpass");
            tag(output, *b"lsct", &bytes)?;
        } else {
            tag(output, *b"lsct", &record.section.to_be_bytes())?;
        }
    }
    for metadata in &record.metadata {
        if metadata.key != super::blend_ranges::TAG {
            tag(output, metadata.key, metadata.bytes)?;
        }
    }
    Ok(())
}
/// Both emission and independent comparison use the same bounded preparation
/// envelope even when the caller admits a larger transfer buffer.
fn stripe_rows(scratch_bytes: usize, width: u32, depth: u16) -> u32 {
    let row_bytes = width as usize * usize::from(depth / 8);
    if row_bytes == 0 {
        return 0;
    }
    (scratch_bytes / row_bytes)
        .min(256)
        .min((128 * 1024 / width.max(1)) as usize) as u32
}

fn tag_start(output: &mut impl Write, key: [u8; 4], bytes: u64) -> Result<(), String> {
    output.write_all(b"8BIM").map_err(|e| e.to_string())?;
    output.write_all(&key).map_err(|e| e.to_string())?;
    output
        .write_all(
            &u32::try_from(bytes)
                .map_err(|_| "Source36 ordinary tag length overflow")?
                .to_be_bytes(),
        )
        .map_err(|e| e.to_string())
}
fn tag(output: &mut impl Write, key: [u8; 4], bytes: &[u8]) -> Result<(), String> {
    tag_start(output, key, bytes.len() as u64)?;
    for bytes in bytes.chunks(65536) {
        output.write_all(bytes).map_err(|e| e.to_string())?;
    }
    pad(output, bytes.len() as u64, 2)
}
fn bytes32(output: &mut impl Write, bytes: &[u8]) -> Result<(), String> {
    output
        .write_all(
            &u32::try_from(bytes.len())
                .map_err(|_| "Source36 block length overflow")?
                .to_be_bytes(),
        )
        .map_err(|e| e.to_string())?;
    output.write_all(bytes).map_err(|e| e.to_string())
}
fn length(output: &mut impl Write, bytes: u64, format: Format) -> Result<(), String> {
    if format == Format::Psb {
        output
            .write_all(&bytes.to_be_bytes())
            .map_err(|e| e.to_string())
    } else {
        output
            .write_all(
                &u32::try_from(bytes)
                    .map_err(|_| "PSD channel length requires PSB")?
                    .to_be_bytes(),
            )
            .map_err(|e| e.to_string())
    }
}
fn pad(output: &mut impl Write, bytes: u64, align: u64) -> Result<(), String> {
    let zeros = [0; 4];
    output
        .write_all(&zeros[..((align - bytes % align) % align) as usize])
        .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::psd::{row_stream::Source, structural};
    use std::io::Cursor;
    #[test]
    fn row_preparation_stays_inside_pixel_and_transfer_bounds_for_native_depths() {
        for depth in [8, 16] {
            for width in [1, 255, 256, 513, 8192, 16000] {
                let row_bytes = width as usize * usize::from(depth / 8);
                for scratch in [0, row_bytes - 1, row_bytes, 512 * 1024] {
                    let rows = stripe_rows(scratch, width, depth);
                    assert!(rows <= 256);
                    assert!(u64::from(rows) * u64::from(width) <= 128 * 1024);
                    assert!(rows as usize * row_bytes <= scratch);
                    assert_eq!(rows == 0, scratch < row_bytes);
                }
            }
        }
        assert_eq!(stripe_rows(512 * 1024, 16000, 8), 8);
        assert_eq!(stripe_rows(512 * 1024, 16000, 16), 8);
    }
    fn header(format: Format, depth: u16) -> Header {
        Header {
            format,
            width: 3,
            height: 257,
            depth,
            channels: 4,
        }
    }
    fn record<'a>() -> Record<'a> {
        Record {
            source: RowSource::Layer(0),
            bounds: [0, 0, 3, 257],
            mask: Some(Mask {
                bounds: [-1, -2, 2, 255],
                enabled: false,
                linked: true,
                default: 255,
                dual: false,
                density: None,
            }),
            name: "Native 漆",
            blend: *b"norm",
            opacity: 255,
            clipped: false,
            visible: true,
            locked: false,
            invert: false,
            tone: None,
            solid: None,
            standard_style: None,
            gradient: None,
            pattern: None,
            has_clipped_layers: false,
            section: 0,
            metadata: vec![],
        }
    }
    fn value(channel: i16, x: i32, y: i32) -> u16 {
        if channel == -1 || channel == 3 {
            if x == 0 && y == 0 {
                return 0;
            }
            (31337 + x * 173 + y * 109) as u16
        } else if channel == -2 {
            (27001 + x * 13 + y * 181) as u16
        } else {
            (10001 + x * 113 + y * 211 + i32::from(channel) * 997) as u16
        }
    }
    struct Provider {
        binding: Digest,
        calls: usize,
        fail: Option<usize>,
        swap: bool,
    }
    impl NativeRows for Provider {
        fn provenance(&self) -> Digest {
            self.binding
        }
        fn fill(
            &mut self,
            r: RowRequest,
            out: &mut [u8],
            check: &dyn Fn() -> Result<(), String>,
        ) -> Result<(), String> {
            check()?;
            self.calls += 1;
            if self.fail == Some(self.calls) {
                return Err("Injected native provider failure".into());
            }
            assert!((1..=256).contains(&r.rows));
            assert_eq!(r.provenance, self.binding);
            let w = (r.bounds[2] - r.bounds[0]) as usize;
            assert_eq!(out.len(), w * r.rows as usize * usize::from(r.depth / 8));
            for row in 0..r.rows {
                for x in 0..w {
                    let xx = r.bounds[0] + x as i32;
                    let yy = r.bounds[1] + (r.first_row + row) as i32;
                    let mut n = value(r.channel, xx, yy);
                    if r.source == RowSource::MergedWhiteMatte && r.channel < 3 {
                        let alpha = value(-1, xx, yy);
                        if r.depth == 16 {
                            n = ((u64::from(n) * u64::from(alpha)
                                + 65535 * (65535 - u64::from(alpha))
                                + 32767)
                                / 65535) as u16;
                        } else {
                            let a = u32::from(alpha / 257);
                            let c = u32::from(n / 257);
                            n = (((c * a + 255 * (255 - a) + 127) / 255) as u16) * 257;
                        }
                    }
                    let i = (row as usize * w + x) * usize::from(r.depth / 8);
                    if r.depth == 16 {
                        out[i..i + 2].copy_from_slice(&n.to_be_bytes());
                    } else {
                        out[i] = (n / 257) as u8;
                    }
                }
            }
            if self.swap {
                self.binding = [99; 32];
            }
            Ok(())
        }
    }
    fn assembled(header: Header, layers: &[u8], composite: &[u8]) -> Vec<u8> {
        let mut out = b"8BPS".to_vec();
        out.extend_from_slice(
            &(if header.format == Format::Psb {
                2u16
            } else {
                1
            })
            .to_be_bytes(),
        );
        out.extend_from_slice(&[0; 6]);
        out.extend_from_slice(&4u16.to_be_bytes());
        out.extend_from_slice(&header.height.to_be_bytes());
        out.extend_from_slice(&header.width.to_be_bytes());
        out.extend_from_slice(&header.depth.to_be_bytes());
        out.extend_from_slice(&3u16.to_be_bytes());
        out.extend_from_slice(&[0; 8]);
        length(&mut out, layers.len() as u64, header.format).unwrap();
        out.extend_from_slice(layers);
        out.extend_from_slice(composite);
        out
    }
    #[test]
    fn raw_standard_native8_native16_psd_psb_masks_and_merged_rows_roundtrip_exactly() {
        for format in [Format::Psd, Format::Psb] {
            for depth in [8, 16] {
                let header = header(format, depth);
                let check = || Ok(());
                let plan =
                    Plan::new(header, [17; 32], vec![record()], &[], &[], Default::default(), &[], &check).unwrap();
                let mut provider = Provider {
                    binding: [17; 32],
                    calls: 0,
                    fail: None,
                    swap: false,
                };
                let mut scratch = vec![0; MAX_TILE_BYTES];
                let mut layers = vec![];
                let mut composite = vec![];
                let mut layer_progress = Vec::new();
                let mut composite_progress = Vec::new();
                let written = plan
                    .write_layers(
                        &mut layers,
                        &mut provider,
                        &mut scratch,
                        &check,
                        |completed, total| {
                            assert_eq!(total, 5 * 257);
                            layer_progress.push(completed);
                            Ok(())
                        },
                        |_, _| unreachable!(),
                    )
                    .unwrap();
                let merged = plan
                    .write_composite(
                        &mut composite,
                        &mut provider,
                        &mut scratch,
                        &check,
                        |completed, total| {
                            assert_eq!(total, 4 * 257);
                            composite_progress.push(completed);
                            Ok(())
                        },
                    )
                    .unwrap();
                let completed = [0, 256, 257, 513, 514, 770, 771, 1027, 1028, 1284, 1285];
                assert_eq!(layer_progress, completed);
                assert_eq!(composite_progress, &completed[..9]);
                assert_eq!(written.bytes, layers.len() as u64);
                assert_eq!(merged.bytes, composite.len() as u64);
                assert_eq!(provider.calls, 18);
                let bytes = assembled(header, &layers, &composite);
                let index = structural::index(
                    &mut Cursor::new(&bytes),
                    structural::Limits::current(),
                    || Ok(()),
                )
                .unwrap();
                assert_eq!(index.layers.len(), 1);
                assert_eq!(index.layers[0].mask.bytes, 20);
                assert_eq!(index.layers[0].channels.len(), 5);
                assert_eq!(
                    bytes[index.layers[0].mask.offset as usize + 17] & 2,
                    2,
                    "disabled state is metadata, not unit replacement pixels"
                );
                let source = Source::new(Cursor::new(&bytes), bytes.len() as u64).unwrap();
                for c in &index.layers[0].channels {
                    let bounds = if c.id == -2 {
                        [-1, -2, 2, 255]
                    } else {
                        [0, 0, 3, 257]
                    };
                    let mut rows = source.rows(c.encoding.as_ref().unwrap(), &check).unwrap();
                    let mut row = vec![0; 3 * usize::from(depth / 8)];
                    for y in 0..257 {
                        assert!(rows.read_row(&mut row).unwrap());
                        for x in 0..3 {
                            let n = value(c.id, bounds[0] + x, bounds[1] + y);
                            if depth == 16 {
                                assert_eq!(
                                    &row[x as usize * 2..x as usize * 2 + 2],
                                    &n.to_be_bytes()
                                );
                            } else {
                                assert_eq!(row[x as usize], (n / 257) as u8);
                            }
                        }
                    }
                    rows.finish()
                        .unwrap()
                        .check(&source, c.encoding.as_ref().unwrap(), &check)
                        .unwrap();
                }
                let mut merged = source.rows(&index.composite, &check).unwrap();
                let mut row = vec![0; 3 * usize::from(depth / 8)];
                for channel in 0..4 {
                    for y in 0..257 {
                        assert!(merged.read_row(&mut row).unwrap());
                        for x in 0..3 {
                            let straight = value(channel, x, y);
                            let alpha = value(-1, x, y);
                            if depth == 16 {
                                let n = if channel < 3 {
                                    ((u64::from(straight) * u64::from(alpha)
                                        + 65535 * (65535 - u64::from(alpha))
                                        + 32767)
                                        / 65535) as u16
                                } else {
                                    alpha
                                };
                                assert_eq!(
                                    &row[x as usize * 2..x as usize * 2 + 2],
                                    &n.to_be_bytes()
                                );
                            } else {
                                let n = if channel < 3 {
                                    let a = u32::from(alpha / 257);
                                    let c = u32::from(straight / 257);
                                    ((c * a + 255 * (255 - a) + 127) / 255) as u8
                                } else {
                                    (alpha / 257) as u8
                                };
                                assert_eq!(row[x as usize], n);
                            }
                        }
                    }
                }
                merged
                    .finish()
                    .unwrap()
                    .check(&source, &index.composite, &check)
                    .unwrap();
                let mut progress = Vec::new();
                let proof = plan
                    .compare(
                        &source,
                        &index,
                        &mut provider,
                        &mut scratch,
                        &mut [0; 32000],
                        &check,
                        |done, total| {
                            progress.push((done, total));
                            Ok(())
                        },
                    )
                    .unwrap();
                proof.check(&plan, &check).unwrap();
                assert_eq!(proof.samples(), 9 * 3 * 257);
                assert_eq!(progress.first(), Some(&(0, 9 * 257)));
                assert_eq!(progress.last(), Some(&(9 * 257, 9 * 257)));
                assert!(progress.windows(2).all(|p| p[0].0 < p[1].0 && p[0].1 == p[1].1));
                let mut other = record();
                other.name = "Different admitted metadata";
                let other =
                    Plan::new(header, [17; 32], vec![other], &[], &[], Default::default(), &[], &check).unwrap();
                assert!(proof.check(&other, &check).is_err());
                let mut changed = bytes.clone();
                let encoded = index.layers[0].channels[0].encoding.as_ref().unwrap();
                changed[encoded.payload.offset as usize] ^= 1;
                let changed_source =
                    Source::new(Cursor::new(&changed), changed.len() as u64).unwrap();
                assert!(
                    plan.compare(
                        &changed_source,
                        &index,
                        &mut provider,
                        &mut scratch,
                        &mut [0; 32000],
                        &check,
                        |_, _| Ok(()),
                    )
                    .is_err(),
                    "Valid streams with changed low bits still fail exact render/source fidelity"
                );
            }
        }
    }
    #[test]
    fn wrong_provenance_provider_failure_changed_snapshot_and_short_transfer_never_complete() {
        let check = || Ok(());
        let plan = Plan::new(
            header(Format::Psb, 16),
            [17; 32],
            vec![record()],
            &[],
            &[],
            Default::default(),
            &[],
            &check,
        )
        .unwrap();
        for variant in 0..4 {
            let mut provider = Provider {
                binding: if variant == 0 { [99; 32] } else { [17; 32] },
                calls: 0,
                fail: if variant == 1 { Some(3) } else { None },
                swap: variant == 2,
            };
            let mut output = vec![];
            let mut scratch = vec![0; if variant == 3 { 5 } else { MAX_TILE_BYTES }];
            assert!(plan
                .write_layers(
                    &mut output,
                    &mut provider,
                    &mut scratch,
                    &check,
                    |_, _| Ok(()),
                    |_, _| unreachable!()
                )
                .is_err());
            if variant == 0 {
                assert!(output.is_empty());
            }
        }
    }
    #[test]
    fn derived_standard_record_admission_does_not_raise_legacy_layer_limits() {
        let check = || Ok(());
        let mut records = Vec::new();
        for _ in 0..MAX_STANDARD_RECORDS {
            let mut r = record();
            r.bounds = [0; 4];
            r.mask = None;
            r.source = RowSource::Zero;
            records.push(r);
        }
        let plan = Plan::new(
            header(Format::Psb, 8),
            [17; 32],
            records,
            &[],
            &[],
            Default::default(),
            &[],
            &check,
        )
        .unwrap();
        let mut provider = Provider {
            binding: [17; 32],
            calls: 0,
            fail: None,
            swap: false,
        };
        let mut layers = vec![];
        let mut composite = vec![];
        let mut scratch = vec![0; MAX_TILE_BYTES];
        plan.write_layers(
            &mut layers,
            &mut provider,
            &mut scratch,
            &check,
            |_, _| Ok(()),
            |_, _| unreachable!(),
        )
        .unwrap();
        plan.write_composite(&mut composite, &mut provider, &mut scratch, &check, |_, _| Ok(()))
            .unwrap();
        let bytes = assembled(plan.header(), &layers, &composite);
        assert_eq!(structural::Limits::current().layers, 200);
        assert!(structural::index(
            &mut Cursor::new(&bytes),
            structural::Limits::current(),
            || Ok(())
        )
        .is_err());
        let mut explicit = structural::Limits::current();
        explicit.layers = MAX_STANDARD_RECORDS;
        let index = structural::index(&mut Cursor::new(&bytes), explicit, || Ok(())).unwrap();
        assert_eq!(index.layers.len(), MAX_STANDARD_RECORDS);
    }
    #[test]
    fn explicit_source_mask_edge_admission_keeps_valid_maximum_origins_without_legacy_change() {
        let check = || Ok(());
        let mut r = record();
        r.bounds = [100000, 100000, 100003, 100257];
        r.mask = Some(Mask {
            bounds: r.bounds,
            enabled: false,
            linked: true,
            default: 255,
            dual: false,
            density: None,
        });
        let header = header(Format::Psb, 16);
        let plan = Plan::new(header, [17; 32], vec![r], &[], &[], Default::default(), &[], &check).unwrap();
        let mut provider = Provider {
            binding: [17; 32],
            calls: 0,
            fail: None,
            swap: false,
        };
        let mut scratch = vec![0; MAX_TILE_BYTES];
        let mut layers = vec![];
        let mut composite = vec![];
        plan.write_layers(
            &mut layers,
            &mut provider,
            &mut scratch,
            &check,
            |_, _| Ok(()),
            |_, _| unreachable!(),
        )
        .unwrap();
        plan.write_composite(&mut composite, &mut provider, &mut scratch, &check, |_, _| Ok(()))
            .unwrap();
        let bytes = assembled(header, &layers, &composite);
        assert_eq!(structural::Limits::current().coordinate, 100000);
        assert!(structural::index(
            &mut Cursor::new(&bytes),
            structural::Limits::current(),
            || Ok(())
        )
        .is_err());
        let mut explicit = structural::Limits::current();
        explicit.coordinate = 116000;
        let index = structural::index(&mut Cursor::new(&bytes), explicit, || Ok(())).unwrap();
        let source = Source::new(Cursor::new(&bytes), bytes.len() as u64).unwrap();
        plan.compare(
            &source,
            &index,
            &mut provider,
            &mut scratch,
            &mut [0; 32000],
            &check,
            |_, _| Ok(()),
        )
        .unwrap();
    }
}
