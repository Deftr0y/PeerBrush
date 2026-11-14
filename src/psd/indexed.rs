//! Encoded-source streaming at the current admission and decoded-raster limits.
//!
//! Metadata is a compact parser view, never the bytes used to bind private
//! definitions. The original source stays borrowed until every stream and the
//! final whole-source observation complete.
use super::{direct_layers::DirectLayers, row_stream, structural, *};
use crate::file_safety::{NATIVE_FILE_BYTES, RESIDENT_FILE_BYTES};
use std::sync::Arc;

struct Input<'a, 'backing, R> {
    source: row_stream::Source<&'a mut R>,
    index: structural::Index,
    embedded: Option<structural::Span>,
    section_hash: u64,
    backing: Option<&'a mut DirectLayers<'backing>>,
}

fn append<R: Read + Seek>(
    source: &row_stream::Source<R>,
    span: structural::Span,
    out: &mut Vec<u8>,
    control: &crate::loading::Control,
) -> Result<(), String> {
    // The native file envelope does not widen the compact metadata buffers.
    let count = usize::try_from(span.bytes).map_err(|_| "Photoshop metadata size overflow")?;
    let end = out
        .len()
        .checked_add(count)
        .filter(|n| *n as u64 <= RESIDENT_FILE_BYTES)
        .ok_or("Photoshop metadata exceeds current admission")?;
    out.try_reserve_exact(count)
        .map_err(|_| "Photoshop metadata allocation failed")?;
    let start = out.len();
    out.resize(end, 0);
    let check = || control.check();
    source
        .range(span, &check)?
        .read_exact(&mut out[start..])
        .map_err(|e| e.to_string())?;
    control.check()
}

fn hash_range<R: Read + Seek>(
    source: &row_stream::Source<R>,
    span: structural::Span,
    control: &crate::loading::Control,
) -> Result<u64, String> {
    let mut bytes = [0; 65536];
    let mut result = 0xcbf29ce484222325;
    source.scan(span, &mut bytes, &|| control.check(), |bytes| {
        hash_part(&mut result, bytes);
        Ok(())
    })?;
    Ok(result)
}

fn source_hash<R: Read + Seek>(
    source: &row_stream::Source<R>,
    count: u64,
    control: &crate::loading::Control,
) -> Result<[u8; 32], String> {
    source.unchanged_length()?;
    let mut bytes = [0; 65536];
    let mut hash = ring::digest::Context::new(&ring::digest::SHA256);
    source.scan(structural::Span { offset: 0, bytes: count }, &mut bytes, &|| control.check(), |bytes| {
        hash.update(bytes);
        Ok(())
    })?;
    source.unchanged_length()?;
    Ok(crate::file_safety::finish_fingerprint(hash))
}

impl<R: Read + Seek> Input<'_, '_, R> {
    fn metadata(
        &mut self,
        layout: Layout,
        control: &crate::loading::Control,
        file_info_oversized: bool,
    ) -> Result<(Vec<u8>, Vec<u8>), String> {
        let mut resources = Vec::new();
        for resource in &self.index.resources {
            control.check()?;
            if file_info::supported(resource.id) {
                if !file_info_oversized {
                    append(&self.source, file_info::record_span(resource)?, &mut resources, control)?;
                }
                continue;
            }
            if resource.peerbrush_source {
                self.embedded = Some(structural::Span {
                    offset: resource.data.offset + 4,
                    bytes: resource.data.bytes - 4,
                });
            }
            let protected_resource = unsupported_document_resource(resource.id).is_some();
            if !protected_resource && !resource.peerbrush_source
                && ![RESOLUTION_RESOURCE_ID, PRINT_SCALE_RESOURCE_ID, 1039,
                    VERSION_INFO_RESOURCE_ID, EFFECTS_VISIBILITY_RESOURCE_ID,
                    PIXEL_ASPECT_RESOURCE_ID, UNTAGGED_PROFILE_RESOURCE_ID,
                    GRID_RESOURCE_ID, LAYER_ID_SEED_RESOURCE_ID,
                    PRINT_FLAGS_RESOURCE_ID, PRINT_FLAGS_INFO_RESOURCE_ID,
                    COPYRIGHT_RESOURCE_ID, GLOBAL_ANGLE_RESOURCE_ID,
                    GLOBAL_ALTITUDE_RESOURCE_ID].contains(&resource.id)
            {
                continue;
            }
            resources.extend_from_slice(b"8BIM");
            u16b(&mut resources, resource.id);
            resources.extend_from_slice(&[0, 0]);
            let bytes = if resource.peerbrush_source {
                4
            } else if protected_resource {
                // Presence alone protects unsupported document structures.
                0
            } else if matches!(resource.id, EFFECTS_VISIBILITY_RESOURCE_ID | UNTAGGED_PROFILE_RESOURCE_ID | COPYRIGHT_RESOURCE_ID) {
                resource.data.bytes.min(2)
            } else if resource.id == VERSION_INFO_RESOURCE_ID {
                resource.data.bytes.min(5)
            } else if resource.id == RESOLUTION_RESOURCE_ID {
                resource.data.bytes.min(17)
            } else if resource.id == PRINT_SCALE_RESOURCE_ID {
                resource.data.bytes.min(15)
            } else if resource.id == PIXEL_ASPECT_RESOURCE_ID {
                resource.data.bytes.min(13)
            } else if resource.id == GRID_RESOURCE_ID {
                resource.data.bytes.min(17)
            } else if matches!(resource.id, LAYER_ID_SEED_RESOURCE_ID | GLOBAL_ANGLE_RESOURCE_ID
                | GLOBAL_ALTITUDE_RESOURCE_ID) {
                resource.data.bytes.min(5)
            } else if resource.id == PRINT_FLAGS_RESOURCE_ID {
                resource.data.bytes.min(10)
            } else if resource.id == PRINT_FLAGS_INFO_RESOURCE_ID {
                resource.data.bytes.min(11)
            } else {
                if resource.data.bytes > crate::color_profile::MAX_PROFILE_BYTES as u64 {
                    return Err("Photoshop ICC profile exceeds the 4 MiB limit".into());
                }
                resource.data.bytes
            };
            u32b(&mut resources, bytes as u32);
            if resource.peerbrush_source {
                resources.extend_from_slice(b"PBR1");
            } else if bytes != 0 {
                append(
                    &self.source,
                    structural::Span {
                        bytes,
                        ..resource.data
                    },
                    &mut resources,
                    control,
                )?;
            }
            if bytes % 2 != 0 {
                resources.push(0);
            }
        }
        let mut active = Vec::new();
        if self.index.active_layer_info.bytes != 0 {
            append(
                &self.source,
                structural::Span {
                    bytes: 2,
                    ..self.index.active_layer_info
                },
                &mut active,
                control,
            )?;
            for layer in &self.index.layers {
                append(&self.source, layer.record, &mut active, control)?;
            }
        }
        let mut section = Vec::new();
        if self.index.layer_and_mask.bytes == 0 {
            return Ok((resources, section));
        }
        if self.index.primary_layer_info == self.index.active_layer_info {
            layout.write_length(&mut section, active.len() as u64)?;
            section.extend_from_slice(&active);
        } else {
            // Preserve the empty count, possible alignment and evidence of any
            // additional primary data without copying inactive layer records.
            let count = self.index.primary_layer_info.bytes.min(5);
            layout.write_length(&mut section, count)?;
            append(
                &self.source,
                structural::Span {
                    bytes: count,
                    ..self.index.primary_layer_info
                },
                &mut section,
                control,
            )?;
        }
        let mask = self.index.global_mask;
        let count = if mask.bytes <= 16 { mask.bytes } else { 1 };
        u32b(&mut section, count as u32);
        append(
            &self.source,
            structural::Span {
                bytes: count,
                ..mask
            },
            &mut section,
            control,
        )?;
        for tag in &self.index.global_tags {
            section.extend_from_slice(&tag.signature);
            section.extend_from_slice(&tag.key);
            let selected = (self.index.header.depth == 8 && tag.key == *b"Layr")
                || (self.index.header.depth == 16 && tag.key == *b"Lr16");
            if selected && tag.data == self.index.active_layer_info {
                layout.write_length_for_tag(&mut section, &tag.key, active.len() as u64)?;
                section.extend_from_slice(&active);
                section.resize(section.len() + (4 - active.len() % 4) % 4, 0);
            } else {
                // Unknown global contents are never interpreted or restored by
                // the current codec. Preserve their emptiness/key/signature so
                // the existing unsupported/duplicate protection is unchanged.
                let count = if selected {
                    tag.data.bytes.min(2)
                } else if matches!(&tag.key, b"Patt" | b"Pat2" | b"Pat3") {
                    tag.data.bytes
                } else if tag.data.bytes <= 4096
                    && std::str::from_utf8(&tag.key)
                        .is_ok_and(|key| super::global_metadata::KEYS.contains(&key))
                {
                    tag.data.bytes
                } else {
                    tag.data.bytes.min(1)
                };
                layout.write_length_for_tag(&mut section, &tag.key, count)?;
                append(
                    &self.source,
                    structural::Span {
                        bytes: count,
                        ..tag.data
                    },
                    &mut section,
                    control,
                )?;
                section.resize(section.len() + (4 - count as usize % 4) % 4, 0);
            }
        }
        Ok((resources, section))
    }

    fn encoding(
        index: &structural::Index,
        record: usize,
        channel: usize,
    ) -> Result<&structural::Encoding, String> {
        index
            .layers
            .get(record)
            .and_then(|r| r.channels.get(channel))
            .ok_or_else(|| "Missing native Photoshop channel".to_string())
            .and_then(|channel| {
                channel.encoding.as_ref().ok_or_else(|| {
                    channel
                        .encoding_error
                        .clone()
                        .unwrap_or_else(|| "Missing native Photoshop channel geometry".into())
                })
            })
    }
}

impl Layout {
    pub(super) fn write_length_for_tag(
        self,
        out: &mut dyn Write,
        key: &[u8],
        bytes: u64,
    ) -> Result<(), String> {
        if self.tag_large(key) {
            out.write_all(&bytes.to_be_bytes())
                .map_err(|e| e.to_string())
        } else {
            let bytes = u32::try_from(bytes).map_err(|_| "Photoshop metadata length overflow")?;
            out.write_all(&bytes.to_be_bytes())
                .map_err(|e| e.to_string())
        }
    }
}

fn write_channel_row(raster: &mut Raster, y: u32, channel: usize, initialize: bool, row: &[u8]) {
    use crate::raster::TILE;
    for tx in 0..raster.width.div_ceil(TILE) {
        let start = tx * TILE;
        let end = (start + TILE).min(raster.width);
        let index = ((y % TILE) * TILE * 4) as usize;
        if raster.depth == 16 {
            let tile = raster
                .samples16
                .entry((tx, y / TILE))
                .or_insert_with(|| Arc::new(vec![0; (TILE * TILE * 4) as usize]));
            let values = Arc::make_mut(tile);
            for x in start..end {
                let i = index + ((x - start) * 4) as usize;
                if initialize {
                    values[i + 3] = 65535;
                }
                values[i + channel] =
                    u16::from_be_bytes([row[x as usize * 2], row[x as usize * 2 + 1]]);
            }
        } else {
            let tile = raster
                .tiles
                .entry((tx, y / TILE))
                .or_insert_with(|| Arc::new(vec![0; (TILE * TILE * 4) as usize]));
            let values = Arc::make_mut(tile);
            for x in start..end {
                let i = index + ((x - start) * 4) as usize;
                if initialize {
                    values[i + 3] = 255;
                }
                values[i + channel] = row[x as usize];
            }
        }
    }
}

impl<R: Read + Seek> IndexedChannels for Input<'_, '_, R> {
    fn same_mask_samples(
        &self, record: usize, first: usize, second: usize, header: MaskHeader, depth: u16,
        control: &crate::loading::Control,
    ) -> Result<bool, String> {
        let first = Self::encoding(&self.index, record, first)?;
        let second = Self::encoding(&self.index, record, second)?;
        let rendered_bounds = header.rendered_bounds.ok_or("Missing rendered Photoshop mask bounds")?;
        let check = || control.check();
        if rendered_bounds != header.bounds {
            let transfer_bytes = first.row_bytes.max(second.row_bytes) as usize;
            let _transfer = self.backing.as_ref()
                .map(|backing| backing.store.resident_budget().reserve_bytes(transfer_bytes, transfer_bytes))
                .transpose()
                .map_err(|error| error.to_string())?;
            let mut row = vec![0; transfer_bytes];
            let mut digest = |encoding: &structural::Encoding, bounds| {
                let row = &mut row[..encoding.row_bytes as usize];
                let mut rows = self.source.rows(encoding, &check)?;
                mask_digest(bounds, header.default, depth, true, |emit| {
                    for y in 0..encoding.rows {
                        if !rows.read_row(row)? {
                            return Err("Missing native Photoshop mask row".into());
                        }
                        emit(y as u32, row);
                    }
                    rows.finish()?.check(&self.source, encoding, &check)
                })
            };
            return Ok(digest(first, rendered_bounds)?.as_ref() == digest(second, header.bounds)?.as_ref());
        }
        if first.rows != second.rows || first.row_bytes != second.row_bytes {
            return Ok(false);
        }
        let row_bytes = first.row_bytes as usize;
        let transfer_bytes = row_bytes.checked_mul(2).ok_or("Photoshop mask row size overflow")?;
        let _transfer = self.backing.as_ref()
            .map(|backing| backing.store.resident_budget().reserve_bytes(transfer_bytes, transfer_bytes))
            .transpose()
            .map_err(|error| error.to_string())?;
        let mut rendered = self.source.rows(first, &check)?;
        let mut user = self.source.rows(second, &check)?;
        let mut a = vec![0; row_bytes];
        let mut b = vec![0; row_bytes];
        for _ in 0..first.rows {
            if !rendered.read_row(&mut a)? || !user.read_row(&mut b)? || a != b {
                return Ok(false);
            }
        }
        rendered.finish()?.check(&self.source, first, &check)?;
        user.finish()?.check(&self.source, second, &check)?;
        Ok(true)
    }

    fn uses_backing(&self) -> bool {
        self.backing.is_some()
    }
    fn reserve_interpreted_bytes(&mut self, bytes: usize) -> Result<(), String> {
        let Some(backing) = self.backing.as_deref_mut() else { return Ok(()); };
        (backing.check)()?;
        let reservation = backing.store.resident_budget().reserve_bytes(bytes, 64 * 1024 * 1024)
            .map_err(|e| e.to_string())?;
        backing.interpreted_charges.try_reserve(1).map_err(|_| "Photoshop metadata reservation allocation failed")?;
        backing.interpreted_charges.push(reservation);
        Ok(())
    }
    fn backed_layer(
        &mut self,
        record: usize,
        layer: &str,
        raster: &Raster,
        require_binary_alpha: bool,
        control: &crate::loading::Control,
    ) -> Result<bool, String> {
        let Some(backing) = self.backing.as_deref_mut() else {
            return Ok(false);
        };
        control.check()?;
        let record = self
            .index
            .layers
            .get(record)
            .ok_or("Missing backed Photoshop layer")?;
        backing.color(&self.source, record, layer, raster, require_binary_alpha)
    }
    fn section_hash(&self) -> u64 {
        self.section_hash
    }
    #[cfg(test)]
    fn embedded_len(&self) -> usize {
        self.embedded.map_or(0, |span| span.bytes as usize)
    }
    fn restore(
        &mut self,
        limit: usize,
        control: &crate::loading::Control,
    ) -> Result<Embedded, String> {
        if self
            .index
            .layers
            .iter()
            .flat_map(|layer| &layer.channels)
            .any(|c| c.encoding_error.is_some())
        {
            return Err("Invalid original standard Photoshop channel".into());
        }
        let check = || control.check();
        let span = self
            .embedded
            .ok_or("Missing original PeerBrush source resource")?;
        let input = self.source.range(span, &check)?;
        embedded_source(ZlibDecoder::new(input), limit, control).map_err(|e| e.to_string())
    }
    fn layer(
        &mut self,
        record: usize,
        channel: usize,
        raster: &mut Raster,
        target: usize,
        initialize: bool,
        control: &crate::loading::Control,
    ) -> Result<(), String> {
        let encoding = Self::encoding(&self.index, record, channel)?;
        if encoding.row_bytes != u64::from(raster.width) * u64::from(raster.depth / 8)
            || encoding.rows != u64::from(raster.height)
        {
            return Err("Native layer geometry differs".into());
        }
        let check = || control.check();
        let mut rows = self.source.rows(encoding, &check)?;
        let mut row = vec![0; encoding.row_bytes as usize];
        for y in 0..raster.height {
            if y % 16 == 0 {
                control.progress(
                    "Decoding native layer pixels",
                    y as usize,
                    raster.height as usize,
                )?;
            }
            if !rows.read_row(&mut row)? {
                return Err("Missing native Photoshop row".into());
            }
            write_channel_row(raster, y, target, initialize, &row);
        }
        rows.finish()?.check(&self.source, encoding, &check)
    }
    fn mask(
        &mut self,
        record: usize,
        channel: usize,
        layer: &str,
        raster: &mut Raster,
        offset: [i32; 2],
        control: &crate::loading::Control,
    ) -> Result<(), String> {
        let encoding = Self::encoding(&self.index, record, channel)?;
        if let Some(backing) = self.backing.as_deref_mut() {
            return backing.mask(&self.source, encoding, layer, raster, offset);
        }
        let high = raster.depth == 16;
        let width = encoding.row_bytes / u64::from(raster.depth / 8);
        let count = width
            .checked_mul(encoding.rows)
            .ok_or("PSD mask size overflow")?;
        if (high && count.checked_mul(2).ok_or("PSD mask size overflow")? > 256 * 1024 * 1024)
            || (!high && count > 32 * 1024 * 1024)
        {
            return Err("PSD mask exceeds decoded-image budget".into());
        }
        let check = || control.check();
        let mut rows = self.source.rows(encoding, &check)?;
        let mut row = vec![0; encoding.row_bytes as usize];
        let left = (-i64::from(offset[0])).clamp(0, width as i64) as u32;
        let right = (i64::from(raster.width) - i64::from(offset[0])).clamp(0, width as i64) as u32;
        for y in 0..encoding.rows {
            #[cfg(test)]
            MASK_ROW_OBSERVER.with(|observer| {
                if let Some(mut observe) = observer.take() {
                    observe(y as u32, row.len(), 0);
                    observer.replace(Some(observe));
                }
            });
            if y % 16 == 0 {
                control.progress(
                    "Decoding native mask pixels",
                    y as usize,
                    encoding.rows as usize,
                )?;
            }
            if !rows.read_row(&mut row)? {
                return Err("Missing native Photoshop mask row".into());
            }
            let yy = y as i64 + i64::from(offset[1]);
            if (0..i64::from(raster.height)).contains(&yy) {
                for x in left..right {
                    let value = if high {
                        u16::from_be_bytes([row[x as usize * 2], row[x as usize * 2 + 1]])
                    } else {
                        u16::from(row[x as usize]) * 257
                    };
                    raster.set16(
                        x as i32 + offset[0],
                        yy as i32,
                        [value, value, value, 65535],
                    );
                }
            }
        }
        rows.finish()?.check(&self.source, encoding, &check)
    }
}

pub(super) fn decode<R: Read + Seek>(
    reader: &mut R,
    control: &crate::loading::Control,
) -> Result<Document, String> {
    decode_bound(reader, control, None).map(|(document, _)| document)
}

#[derive(Clone)]
pub(crate) struct ConventionalComposite {
    header: structural::Header,
    encoding: structural::Encoding,
    file_bytes: u64,
    has_alpha: bool,
}

impl ConventionalComposite {
    pub(crate) fn matches_native(
        &self,
        reader: &mut (impl Read + Seek),
        scene: &crate::engine::scene::SceneSnapshot,
        session: &crate::workspace::source_runtime::SourceSession,
        control: &crate::loading::Control,
        check: &dyn Fn() -> Result<(), String>,
    ) -> Result<bool, String> {
        use super::standard36::{NativeRows, RowRequest, RowSource};
        use crate::large_document::ScratchBytes;
        let structural::Header {
            width,
            height,
            depth,
            channels,
            ..
        } = self.header;
        let metadata = scene.metadata();
        if (metadata.width(), metadata.height(), metadata.depth()) != (width, height, depth) {
            return Ok(false);
        }
        let scratch =
            |bytes| ScratchBytes::new(scene.budget(), bytes, bytes).map_err(|e| e.to_string());
        let sample = usize::from(depth / 8);
        let stride = width as usize * sample;
        if self.encoding.row_bytes != stride as u64 {
            return Err("Photoshop composite row width differs from the canvas".into());
        }
        check()?;
        let cancel = || control.check();
        let packed = channels as usize * (stride * 2 + 1024);
        // Leave room for the RGBA stripe and compositor before reserving
        // additional packed rows for the optional interleaved path.
        let packed_admission = if self.encoding.compression == structural::Compression::Rle
            && scene.budget().limit().saturating_sub(scene.budget().used()) >= packed + 2 * 1024 * 1024
        {
            scene.budget().reserve_bytes(packed, packed).ok()
        } else {
            None
        };
        if self.encoding.compression == structural::Compression::Raw || packed_admission.is_some() {
            let source = row_stream::Source::new(reader, self.file_bytes)?;
            let mut table = [0; 4096];
            let mut planes = source.composite_planes(&self.encoding, height, channels, &mut table, check, &cancel)?;
            if scene.budget().limit().saturating_sub(scene.budget().used()) >= 2 * 1024 * 1024 {
                planes.prefetch(scene.budget());
            }
            let matches = self.matches_planar(scene, session, check, |channel, _, output| {
                planes.read(channel, output)
            })?;
            if matches {
                planes.finish()?;
            }
            return Ok(matches);
        }
        let plane_bytes = stride * height as usize;
        let decoded_bytes = plane_bytes * channels as usize;
        // This optional copy avoids new disk staging. Leave most of the
        // remaining pool available for masks, effects and stripe rendering.
        let headroom = scene.budget().limit().saturating_sub(scene.budget().used());
        if matches!(self.encoding.compression, structural::Compression::Zip | structural::Compression::PredictedZip)
            && decoded_bytes <= (headroom / 4).min(64 * 1024 * 1024)
        {
            if let Ok(mut decoded) = scratch(decoded_bytes) {
                let source = row_stream::Source::new(reader, self.file_bytes)?;
                let mut merged = source.rows_with_checks(&self.encoding, check, &cancel)?;
                for row in decoded.chunks_exact_mut(stride) {
                    if !merged.read_row(row)? {
                        return Err("Missing conventional Photoshop composite row".into());
                    }
                }
                merged.finish()?.check(&source, &self.encoding, check)?;
                return self.matches_planar(scene, session, check, |channel, first, output| {
                    let start = channel * plane_bytes + first as usize * stride;
                    output.copy_from_slice(&decoded[start..start + output.len()]);
                    Ok(())
                });
            }
        }
        let source = row_stream::Source::with_buffer(reader, self.file_bytes, scratch(65536)?)?;
        let mut merged = source.rows_with_checks(&self.encoding, check, &cancel)?;
        let mut expected = scratch(stride)?;
        let mut rendered = scratch(65536)?;
        super::native_rows36::with_styles(
            scene,
            session,
            check,
            |rows| {
                let provenance = rows.provenance();
                let stripe = ((rendered.len() / stride) as u32).min(256);
                for channel in 0..4u16 {
                    let mut first = 0;
                    while first < height {
                        check()?;
                        let count = stripe.min(height - first);
                        let pixels = &mut rendered[..stride * count as usize];
                        rows.fill(
                            RowRequest {
                                provenance,
                                source: RowSource::MergedWhiteMatte,
                                channel: channel as i16,
                                bounds: [0, 0, width as i32, height as i32],
                                first_row: first,
                                rows: count,
                                depth,
                            },
                            pixels,
                            check,
                        )?;
                        for row in pixels.chunks_exact(stride) {
                            if channel < channels && !merged.read_row(&mut expected)? {
                                return Err("Missing conventional Photoshop composite row".into());
                            }
                            if channel == 3 && !self.has_alpha {
                                if row.iter().any(|&sample| sample != 255) {
                                    return Ok(false);
                                }
                            } else if row != &*expected {
                                return Ok(false);
                            }
                        }
                        first += count;
                    }
                }
                merged.finish()?.check(&source, &self.encoding, check)?;
                check()?;
                Ok(true)
            },
            |_| Ok(()),
        )
    }

    fn matches_planar(
        &self,
        scene: &crate::engine::scene::SceneSnapshot,
        session: &crate::workspace::source_runtime::SourceSession,
        check: &dyn Fn() -> Result<(), String>,
        mut read: impl FnMut(usize, u32, &mut [u8]) -> Result<(), String>,
    ) -> Result<bool, String> {
        let sample = usize::from(self.header.depth / 8);
        let stride = self.header.width as usize * sample;
        let height = self.header.height;
        let stripe = ((65536 / stride) as u32).min(256).min(height);
        let capacity = stride * stripe as usize;
        let scratch = |bytes| crate::large_document::ScratchBytes::new(scene.budget(), bytes, bytes)
            .map_err(|e| e.to_string());
        let mut expected = scratch(capacity)?;
        let mut rgba = scratch(capacity * 4)?;
        super::native_rows36::with_styles(scene, session, check, |rows| {
            for first in (0..height).step_by(stripe as usize) {
                let count = stripe.min(height - first);
                let bytes = stride * count as usize;
                let rgba = &mut rgba[..bytes * 4];
                rows.fill_composite_rgba(first, count, true, rgba, check)?;
                for channel in 0..self.header.channels as usize {
                    let expected = &mut expected[..bytes];
                    read(channel, first, expected)?;
                    if (channel != 3 || self.has_alpha)
                        && rgba.chunks_exact(sample * 4).zip(expected.chunks_exact(sample))
                            .any(|(pixel, word)| &pixel[channel * sample..(channel + 1) * sample] != word)
                    {
                        return Ok(false);
                    }
                }
                if !self.has_alpha && rgba.chunks_exact(sample * 4)
                    .any(|pixel| pixel[3 * sample..].iter().any(|&byte| byte != 255))
                {
                    return Ok(false);
                }
            }
            check()?;
            Ok(true)
        }, |_| Ok(()))
    }
}

pub(super) fn decode_bound<R: Read + Seek>(
    reader: &mut R,
    control: &crate::loading::Control,
    expected: Option<[u8; 32]>,
) -> Result<(Document, Option<ConventionalComposite>), String> {
    decode_with(reader, control, expected, None, false)
}

pub(super) fn decode_unpaired<R: Read + Seek>(
    reader: &mut R,
    control: &crate::loading::Control,
    expected: [u8; 32],
) -> Result<Document, String> {
    decode_with(reader, control, Some(expected), None, true).map(|(doc, _)| doc)
}

pub(crate) struct DecodedBacked {
    pub(crate) document: Document,
    pub(crate) composite: ConventionalComposite,
    pub(crate) color: std::collections::BTreeMap<String, crate::raster::Surface>,
    pub(crate) masks: std::collections::BTreeMap<String, crate::raster::Surface>,
    pub(crate) mask_originals: std::collections::BTreeMap<String, crate::raster::Surface>,
    _working: [Arc<super::memory36::MetadataCharge>; 2],
    _interpreted: Vec<crate::large_document::Reservation>,
}

pub(crate) struct ProtectedComposite {
    pub(crate) surface: crate::raster::Surface,
    pub(crate) profile: Option<Vec<u8>>,
    pub(crate) structure: Option<ProtectedStructure>,
    pub(crate) notice: Option<&'static str>,
    pub(crate) srgb_tagged: bool,
    pub(crate) document_resources: DocumentResources,
    pub(crate) file_info: Option<crate::engine::scene::MetadataPayload<Vec<u8>>>,
    pub(crate) resource_warnings: Vec<String>,
    _metadata: Arc<super::memory36::MetadataCharge>,
}

const GRAYSCALE_NOTICE: &str = "Photoshop grayscale layers are protected. Showing the saved grayscale composite at its original channel depth";
const PROFILED_GRAYSCALE_NOTICE: &str = "Photoshop grayscale layers are protected. The saved composite is displayed in sRGB at its original channel depth using the original monochrome ICC profile";

fn indexed_structure<R: Read + Seek>(
    source: &row_stream::Source<R>,
    index: &structural::Index,
    pool: &crate::large_document::ResidentBudget,
    check: &dyn Fn() -> Result<(), String>,
) -> Result<Option<ProtectedStructure>, String> {
    // At most 1,024 UTF-16 name units, 64 tags and bounded fixed diagnostics per layer.
    let charge = match memory36::MetadataCharge::reserve(pool, 8192 * index.layers.len(), check) {
        Ok(charge) => charge,
        Err(_) => {
            check()?;
            return Ok(None);
        }
    };
    let mut records = Vec::with_capacity(index.layers.len());
    let mut parents = Vec::new();
    let mut unbalanced = false;
    for (source_index, layer) in index.layers.iter().enumerate().rev() {
        check()?;
        let mut warnings = Vec::new();
        let mut dividers = layer.tags.iter().filter(|tag| matches!(&tag.key, b"lsct" | b"lsdk"));
        let mut section = 0;
        if let Some(tag) = dividers.next() {
            if dividers.next().is_none() && matches!(tag.data.bytes, 4 | 12 | 16) {
                let mut bytes = [0; 16];
                let payload = &mut bytes[..tag.data.bytes as usize];
                source.range(tag.data, check)?.read_exact(payload).map_err(|e| e.to_string())?;
                match section_divider(&tag.signature, payload) {
                    Ok((kind, _)) => section = kind,
                    Err(reason) => {
                        warnings.push(reason);
                        unbalanced = true;
                    }
                }
            } else {
                unbalanced = true;
            }
        }
        if section == 3 {
            unbalanced |= parents.pop().is_none();
            continue;
        }
        let mut name_bytes = [0; 255];
        let name_len = layer.name.bytes as usize;
        let legacy = name_bytes.get_mut(..name_len).ok_or("Photoshop layer name exceeds its index bound")?;
        source.range(layer.name, check)?.read_exact(legacy).map_err(|e| e.to_string())?;
        let mut name = String::from_utf8_lossy(legacy).into_owned();
        let mut details_truncated = layer.tags.len() > 64;
        let mut unicode = layer.tags.iter().filter(|tag| tag.key == *b"luni");
        if let Some(tag) = unicode.next() {
            if unicode.next().is_none() && tag.signature == *b"8BIM"
                && tag.data.bytes <= MAX_LAYER_NAME_BYTES as u64
            {
                let mut bytes = [0; MAX_LAYER_NAME_BYTES];
                let payload = &mut bytes[..tag.data.bytes as usize];
                source.range(tag.data, check)?.read_exact(payload).map_err(|e| e.to_string())?;
                match unicode_layer_name(payload, 1024) {
                    Ok((decoded, truncated)) => {
                        name = decoded;
                        details_truncated |= truncated;
                    }
                    Err(error) => warnings.push(format!("{error}; showing the legacy name")),
                }
            } else {
                warnings.push("Ambiguous Photoshop Unicode layer name; showing the legacy name".into());
            }
        }
        let [top, left, bottom, right] = layer.bounds;
        let mut tags = std::collections::BTreeSet::new();
        for tag in &layer.tags {
            tags.insert(tag.key);
            if tags.len() > 64 { tags.pop_last(); }
            if let Some(reason) = unsupported_layer_source(&tag.key) {
                if !warnings.iter().any(|warning| warning == reason) {
                    warnings.push(reason.into());
                }
            }
        }
        let mut masks = layer.tags.iter().filter(|tag| matches!(&tag.key, b"vmsk" | b"vsms"));
        if let Some(tag) = masks.next() {
            let warning = if masks.next().is_some() {
                Some("Multiple Photoshop vector mask records remain protected".into())
            } else if tag.signature != *b"8BIM" {
                Some("Invalid Photoshop vector mask signature".into())
            } else if tag.data.bytes > vector_mask::MAX_BYTES as u64 {
                Some("Photoshop vector mask exceeds the inspection limit".into())
            } else if let Ok(mut bytes) = crate::large_document::ScratchBytes::new(
                pool, tag.data.bytes as usize, vector_mask::MAX_BYTES,
            ) {
                source.range(tag.data, check)?.read_exact(&mut bytes).map_err(|e| e.to_string())?;
                match vector_mask::inspect(&bytes) {
                    Ok(mask) if tag.key == *b"vmsk" && mask.is_noop() => None,
                    Ok(mask) => Some(mask.protection_reason(&tag.key)),
                    Err(reason) => Some(format!("Unsupported Photoshop vector mask: {reason}")),
                }
            } else {
                details_truncated = true;
                Some("Photoshop vector mask details exceed the available inspection budget".into())
            };
            warnings.extend(warning);
        }
        if layer.channels.iter().any(|channel| channel.encoding_error.is_some()) {
            warnings.push("One or more Photoshop layer channels could not be indexed".into());
        }
        if index.ambiguities.iter().any(|ambiguity| matches!(ambiguity,
            structural::Ambiguity::LayerTag(layer, _) | structural::Ambiguity::LayerChannel(layer, _)
                if *layer == source_index))
        {
            warnings.push("Duplicate Photoshop layer tags or channels".into());
        }
        let group = matches!(section, 1 | 2);
        records.push(SourceLayerInfo {
            source_index,
            parent_index: parents.last().copied(),
            name,
            kind: if group { "group" } else { "layer" }.into(),
            bounds: [left, top, right, bottom],
            visible: layer.flags & 2 == 0,
            has_mask: layer.mask.bytes != 0 || layer.channels.iter().any(|c| matches!(c.id, -2 | -3))
                || layer.tags.iter().any(|tag| matches!(&tag.key, b"vmsk" | b"vsms")),
            tags: tags.into_iter().map(|key| String::from_utf8_lossy(&key).into_owned()).collect(),
            warnings,
            details_truncated,
        });
        if group { parents.push(source_index); }
    }
    if unbalanced || !parents.is_empty() {
        for layer in &mut records { layer.parent_index = None; }
        if let Some(layer) = records.first_mut() {
            layer.warnings.push("Photoshop folder boundaries are ambiguous; hierarchy is unavailable".into());
        }
    }
    Ok(Some(ProtectedStructure::from_parts(Arc::new(records), charge)))
}

pub(crate) fn decode_protected<R: Read + Seek>(
    reader: &mut R,
    version: crate::file_safety::Version,
    session: &crate::workspace::source_runtime::SourceSession,
    store: crate::large_document::Store,
    control: &crate::loading::Control,
    check: &dyn Fn() -> Result<(), String>,
) -> Result<ProtectedComposite, String> {
    use crate::large_document::{ByteRole, Error, ScratchBytes};
    let pool = session.resident();
    let metadata = super::memory36::MetadataCharge::reserve(&pool, 12 * 1024 * 1024, check)?;
    let initial = row_stream::Source::new(&mut *reader, version.bytes)?;
    if source_hash(&initial, version.bytes, control)? != version.sha256() {
        return Err("Photoshop preview source bytes changed".into());
    }
    let index = structural::index_protected(reader, check)?;
    if index.file_bytes != version.bytes {
        return Err("Photoshop preview source length changed".into());
    }
    let source = row_stream::Source::new(reader, version.bytes)?;
    let mut profile = preview_profile(&source, &index, control, check)?;
    let (document_resources, mut resource_warnings) = inspect_indexed_resources(&source, &index, check)?;
    let file_info = {
        let working = file_info::descriptor_working_bytes(&index)? as usize;
        let _charge = super::memory36::MetadataCharge::reserve(&pool, working, check)?;
        file_info::inspect_indexed(&source, &index, &mut resource_warnings, check)?
            .map(crate::engine::scene::MetadataPayload::from)
    };
    resource_warnings.extend(indexed_document_resource_warnings(&source, &index, check)?);
    let structural::Header { width, height, depth, channels, .. } = index.header;
    let color_channels = index.color_channels;
    let gray_table = if color_channels == 1 {
        profile.take().as_deref()
            .map(|profile| crate::color_profile::prepared::GrayTable::new(profile, &pool, check)).transpose()?
    } else {
        None
    };
    let sample_bytes = usize::from(depth / 8);
    let stride = width as usize * sample_bytes;
    let plane = stride as u64 * u64::from(height);
    let has_alpha = channels > color_channels && index.merged_transparency;
    let planes = u64::from(color_channels) + u64::from(has_alpha);
    let scratch = |bytes| ScratchBytes::new(&pool, bytes, bytes).map_err(|e| e.to_string());
    let strip_rows = (65536 / stride) as u32;
    let stage = |read: &mut dyn FnMut(u64, u32, &mut [u8]) -> Result<(), String>| {
        let mut rows = crate::selection::NativeRows::new(width, height, depth, store)
            .map_err(|e| e.to_string())?;
        let mut rgba = scratch(width as usize * 4 * sample_bytes)?;
        let mut stripes = (0..planes).map(|_| scratch(stride * strip_rows.min(height) as usize))
            .collect::<Result<Vec<_>, _>>()?;
        let max = if depth == 16 { 65535 } else { 255 };
        let mut guarded = || check().map_err(|_| Error::Cancelled);
        for y in (0..height).step_by(strip_rows as usize) {
            control.progress("Staging protected Photoshop preview", y as usize, height as usize)?;
            let count = strip_rows.min(height - y);
            for (channel, stripe) in stripes.iter_mut().enumerate() {
                read(channel as u64, y, &mut stripe[..stride * count as usize])?;
            }
            for row in 0..count {
                check()?;
                for (x, rgba) in rgba.chunks_exact_mut(4 * sample_bytes).enumerate() {
                    let at = row as usize * stride + x * sample_bytes;
                    let mut pixel = [max as u16; 4];
                    let sample = |stripe: &ScratchBytes| {
                        if depth == 16 {
                            u16::from_be_bytes([stripe[at], stripe[at + 1]])
                        } else {
                            u16::from(stripe[at])
                        }
                    };
                    if color_channels == 1 {
                        pixel[..3].fill(sample(&stripes[0]));
                        if has_alpha { pixel[3] = sample(&stripes[1]); }
                    } else {
                        for (value, stripe) in pixel.iter_mut().zip(&stripes) {
                            *value = sample(stripe);
                        }
                    }
                    if i64::from(pixel[3]) != max {
                        restore_white_matte(&mut pixel, max);
                    }
                    if let Some(table) = &gray_table {
                        table.convert(&mut pixel, depth);
                    }
                    if depth == 16 {
                        for (target, sample) in rgba.chunks_exact_mut(2).zip(pixel) {
                            target.copy_from_slice(&sample.to_be_bytes());
                        }
                    } else {
                        rgba.copy_from_slice(&pixel.map(|sample| sample as u8));
                    }
                }
                if depth == 16 {
                    rows.row(y + row, &rgba, &mut guarded)
                } else {
                    rows.row8(y + row, &rgba, &mut guarded)
                }.map_err(|e| e.to_string())?;
            }
        }
        rows.finish(&mut guarded).map_err(|e| e.to_string())
    };
    let surface = if index.composite.compression == structural::Compression::Raw {
        let cancel = || control.check();
        let mut raw = source.composite_planes(&index.composite, height, channels, &mut [], check, &cancel)?;
        let surface = stage(&mut |channel, _, stripe| raw.read(channel as usize, stripe))?;
        if planes < u64::from(channels) {
            let mut ignored = scratch(stride * strip_rows.min(height) as usize)?;
            for y in (0..height).step_by(strip_rows as usize) {
                let bytes = stride * strip_rows.min(height - y) as usize;
                raw.read(planes as usize, &mut ignored[..bytes])?;
            }
        }
        raw.finish()?;
        surface
    } else {
        let mut encoded = source.rows(&index.composite, check)?;
        let mut batch = scratch(stride * strip_rows.min(height) as usize)?;
        let mut spool = session.byte_temporary(plane * planes, ByteRole::CompositeData)?;
        let decoded = (|| {
            for channel in 0..u64::from(channels) {
                for y in (0..height).step_by(strip_rows as usize) {
                    control.progress("Decoding protected Photoshop composite",
                        (channel * u64::from(height) + u64::from(y)) as usize,
                        usize::from(channels) * height as usize)?;
                    let bytes = stride * strip_rows.min(height - y) as usize;
                    encoded.read_rows(&mut batch[..bytes])?;
                    if channel < planes {
                        spool.append(&batch[..bytes], &mut || check())?;
                    }
                }
            }
            encoded.finish()?.check(&source, &index.composite, check)
        })();
        if let Err(reason) = decoded {
            return Err(match spool.abort(&mut || session.cleanup_check()) {
                Ok(()) => reason,
                Err(cleanup) => format!("{reason}; Photoshop composite spool retained: {cleanup}"),
            });
        }
        drop(batch);
        drop(encoded);
        let spool = spool.finish(version.sha256(), &mut || check())?;
        let staged = stage(&mut |channel, y, stripe| {
            spool.check()?;
            spool.bytes().read_range_into(channel * plane + u64::from(y) * stride as u64,
                stripe, &mut || check().map_err(|_| Error::Cancelled)).map_err(|e| e.to_string())
        });
        match (staged, spool.retire(&mut || session.cleanup_check())) {
            (result, Ok(())) => result?,
            (Ok(_), Err(cleanup)) => return Err(format!("Photoshop composite spool retained: {cleanup}")),
            (Err(reason), Err(cleanup)) => return Err(format!("{reason}; Photoshop composite spool retained: {cleanup}")),
        }
    };
    let structure = indexed_structure(&source, &index, &pool, check)?;
    if source_hash(&source, version.bytes, control)? != version.sha256() {
        return Err("Photoshop preview source bytes changed while decoding".into());
    }
    let srgb_tagged = gray_table.is_some();
    Ok(ProtectedComposite { surface, profile, structure, srgb_tagged, document_resources, file_info, resource_warnings,
        notice: (color_channels == 1).then_some(if srgb_tagged { PROFILED_GRAYSCALE_NOTICE } else { GRAYSCALE_NOTICE }),
        _metadata: metadata })
}

fn preview_profile<R: Read + Seek>(
    source: &row_stream::Source<R>,
    index: &structural::Index,
    control: &crate::loading::Control,
    check: &dyn Fn() -> Result<(), String>,
) -> Result<Option<Vec<u8>>, String> {
    if !indexed_saved_composite(source, index, check)? {
        return Err("Photoshop source does not declare a current saved composite".into());
    }
    let mut profile = None;
    for resource in index.resources.iter().filter(|resource| resource.id == 1039) {
        if profile.is_some() || resource.data.bytes > crate::color_profile::MAX_PROFILE_BYTES as u64 {
            return Err("Photoshop preview has an ambiguous or oversized ICC profile".into());
        }
        let mut bytes = Vec::new();
        append(source, resource.data, &mut bytes, control)?;
        profile = Some(bytes);
    }
    Ok(profile)
}

fn resident_composite<R: Read + Seek>(
    source: &row_stream::Source<R>,
    index: &structural::Index,
    control: &crate::loading::Control,
) -> Result<(Raster, bool), String> {
    let structural::Header { width, height, depth, channels, .. } = index.header;
    let color_channels = index.color_channels;
    let mut merged = Raster::new_depth(width, height, depth);
    let encoding = &index.composite;
    let check = || control.check();
    let mut rows = source.rows(encoding, &check)?;
    let mut row = vec![0; encoding.row_bytes as usize];
    for channel in 0..usize::from(channels) {
        for y in 0..height {
            if y % 16 == 0 {
                control.progress("Decoding saved image", channel * height as usize + y as usize,
                    usize::from(channels) * height as usize)?;
            }
            if !rows.read_row(&mut row)? {
                return Err("Missing saved composite row".into());
            }
            if channel == usize::from(color_channels) {
                if index.merged_transparency {
                    write_channel_row(&mut merged, y, 3, false, &row);
                }
            } else if color_channels == 1 {
                for rgb in 0..3 {
                    write_channel_row(&mut merged, y, rgb, rgb == 0, &row);
                }
            } else {
                write_channel_row(&mut merged, y, channel, channel == 0, &row);
            }
        }
    }
    rows.finish()?.check(source, encoding, &check)?;
    restore_saved_composite(merged, control)
}

fn resident_grayscale<R: Read + Seek>(
    source: &row_stream::Source<R>,
    index: &structural::Index,
    before: [u8; 32],
    control: &crate::loading::Control,
) -> Result<Document, String> {
    let check = || control.check();
    let profile = preview_profile(source, index, control, &check)?;
    let (document_resources, mut print_warnings) = inspect_indexed_resources(source, index, &check)?;
    let file_info = file_info::inspect_indexed(source, index, &mut print_warnings, &check)?;
    let table = profile.as_deref().map(|profile| {
        use crate::color_profile::prepared::GrayTable;
        GrayTable::new(profile, &GrayTable::resident_pool()?, &check)
    }).transpose()?;
    let (mut merged, _) = resident_composite(source, index, control)?;
    if let Some(table) = &table {
        for tile in merged.samples16.values_mut() {
            check()?;
            for pixel in Arc::make_mut(tile).chunks_exact_mut(4) {
                table.convert(pixel, 16);
            }
        }
        for tile in merged.tiles.values_mut() {
            check()?;
            for pixel in Arc::make_mut(tile).chunks_exact_mut(4) {
                let mut native = [pixel[0], pixel[1], pixel[2], pixel[3]].map(u16::from);
                table.convert(&mut native, 8);
                pixel.copy_from_slice(&native.map(|value| value as u8));
            }
        }
    }
    let mut doc = Document::new_depth(merged.width, merged.height, merged.depth)?;
    doc.read_only = true;
    doc.srgb_tagged = table.is_some();
    doc.psd_resolution = document_resources.resolution;
    doc.psd_print_scale = document_resources.scale;
    doc.psd_pixel_aspect = document_resources.pixel_aspect;
    doc.psd_grid = document_resources.grid;
    doc.psd_layer_id_seed = document_resources.layer_id_seed;
    doc.psd_print_flags = document_resources.print_flags;
    doc.psd_print_flags_info = document_resources.print_flags_info;
    doc.psd_copyright = document_resources.copyright;
    doc.psd_file_info = file_info.map(Arc::new);
    doc.psd_global_angle = document_resources.global_angle;
    doc.psd_global_altitude = document_resources.global_altitude;
    doc.psd_intentionally_untagged = document_resources.intentionally_untagged;
    doc.warnings.push(if doc.srgb_tagged { PROFILED_GRAYSCALE_NOTICE } else { GRAYSCALE_NOTICE }.into());
    doc.warnings.extend(print_warnings);
    doc.warnings.extend(indexed_document_resource_warnings(source, index, &check)?);
    doc.layers[0].name = "Saved grayscale composite".into();
    doc.layers[0].pixels = merged;
    let budget = crate::large_document::ResidentBudget::new(2 * 1024 * 1024).map_err(|e| e.to_string())?;
    if let Some(records) = indexed_structure(source, index, &budget, &check)? {
        doc.source_structure = (*records).clone();
    }
    if source_hash(source, index.file_bytes, control)? != before {
        return Err("Photoshop source bytes changed during decoding".into());
    }
    control.progress("Ready", 1, 1)?;
    Ok(doc)
}

fn backed_metadata_bytes(index: &structural::Index) -> Result<usize, String> {
    let encoded = index
        .file_bytes
        .checked_mul(2)
        .ok_or("Photoshop metadata footprint overflow")?;
    let mut profiles = index.resources.iter().filter(|resource| resource.id == 1039);
    let profile_working = if let Some(profile) = profiles.next() {
        if profiles.next().is_some() || profile.data.bytes > crate::color_profile::MAX_PROFILE_BYTES as u64 {
            return Err("Duplicate or oversized Photoshop ICC profile".into());
        }
        2 * profile.data.bytes + 65536
    } else { 0 };
    let mut style_working = 0;
    for tag in index.layers.iter().flat_map(|layer| &layer.tags)
        .filter(|tag| matches!(&tag.key, b"lfx2" | b"lmfx"))
    {
        style_working = style_working.max(styles::native_working_bytes(tag.data.bytes)?);
    }
    let mut pattern_keys = std::collections::HashSet::new();
    let mut pattern_bytes = 0;
    for tag in index.global_tags.iter().filter(|tag| matches!(&tag.key, b"Patt" | b"Pat2" | b"Pat3")) {
        if !pattern_keys.insert(tag.key) || tag.data.bytes > patterns::MAX_BLOCK as u64 {
            return Err("Duplicate or oversized Photoshop pattern block".into());
        }
        pattern_bytes += tag.data.bytes;
    }
    // The active layer-info span also includes encoded channels. Only the
    // record headers are copied. Include both compact copies and decoded strings.
    let bytes = index.layers.iter().map(|layer| layer.record.bytes)
        .chain(index.global_tags.iter().filter(|tag| tag.data != index.active_layer_info)
            .map(|tag| tag.data.bytes.min(4096)))
        .try_fold(0u64, u64::checked_add)
        .filter(|bytes| *bytes <= RESIDENT_FILE_BYTES)
        .and_then(|bytes| bytes.checked_mul(4))
        .map(|bytes| bytes.min(encoded));
    // The packed block copies coexist with a single tile's PNG encode and
    // validation scratch. Retained asset and effect bytes are charged per use.
    let pattern_working = if pattern_bytes == 0 { 0 } else { 2 * pattern_bytes + 16 * 1024 * 1024 };
    let file_info_bytes = file_info::indexed_length(index)?;
    let file_info_working = if file_info_bytes <= file_info::MAX_BYTES as u64 {
        2 * file_info_bytes + file_info::descriptor_working_bytes(index)?
    } else { 0 };
    bytes
        .and_then(|bytes| bytes.checked_add(pattern_working + profile_working + style_working + file_info_working))
        .and_then(|bytes| usize::try_from(bytes).ok())
        .ok_or_else(|| "Photoshop metadata footprint overflow".into())
}

pub(crate) fn decode_backed<R: Read + Seek>(
    reader: &mut R,
    control: &crate::loading::Control,
    version: crate::file_safety::Version,
    store: crate::large_document::Store,
    check: &dyn Fn() -> Result<(), String>,
) -> Result<DecodedBacked, String> {
    if version.bytes > NATIVE_FILE_BYTES {
        return Err("Photoshop native file limit is 64 GiB".into());
    }
    // The bounded index, fixed parser headers and channel transfer buffers
    // coexist with the compact metadata and separately charged native tiles.
    let indexing =
        super::memory36::MetadataCharge::reserve(&store.resident_budget(), 8 * 1024 * 1024, check)?;
    let mut backing = DirectLayers {
        store,
        check,
        control,
        color: Default::default(),
        masks: Default::default(),
        mask_originals: Default::default(),
        metadata_charge: None,
        interpreted_charges: Vec::new(),
    };
    let (document, composite) =
        decode_with(reader, control, Some(version.sha256()), Some(&mut backing), false)?;
    Ok(DecodedBacked {
        document,
        composite: composite.ok_or("Backed Photoshop source has no admissible saved composite")?,
        color: backing.color,
        masks: backing.masks,
        mask_originals: backing.mask_originals,
        _working: [
            indexing,
            backing.metadata_charge.ok_or("Missing Photoshop metadata admission")?,
        ],
        _interpreted: backing.interpreted_charges,
    })
}

fn decode_with<R: Read + Seek>(
    reader: &mut R,
    control: &crate::loading::Control,
    expected: Option<[u8; 32]>,
    mut backing: Option<&mut DirectLayers<'_>>,
    require_peerbrush_source: bool,
) -> Result<(Document, Option<ConventionalComposite>), String> {
    control.progress("Reading PSD header", 0, 1)?;
    let bytes = reader.seek(SeekFrom::End(0)).map_err(|e| e.to_string())?;
    let backed = backing.is_some();
    if backed && bytes > NATIVE_FILE_BYTES {
        return Err("Photoshop native file limit is 64 GiB".into());
    }
    if !backed && bytes > RESIDENT_FILE_BYTES {
        return Err("Photoshop file limit is 256 MiB".into());
    }
    let before = source_hash(
        &row_stream::Source::new(&mut *reader, bytes)?,
        bytes,
        control,
    )?;
    if expected.is_some_and(|expected| before != expected) {
        return Err("Photoshop bytes differ from the held open source".into());
    }
    let index = if backed {
        structural::index(reader, structural::Limits::native_standard(), || control.check())?
    } else {
        structural::index_for_decode(reader, || control.check())?
    };
    if backed {
        if let Some(reason) = index.resources.iter().find_map(|r| unsupported_document_resource(r.id)) {
            return Err(reason.into());
        }
    }
    let mut supplementary = index.resources.iter().filter(|resource| resource.peerbrush_source);
    let has_supplementary = supplementary.next().is_some();
    if require_peerbrush_source && (!has_supplementary || supplementary.next().is_some()) {
        return Err("Unpaired recovery has no unique PeerBrush source; the file is retained".into());
    }
    if backed && has_supplementary {
        return Err("Supplementary Photoshop sources require compatibility decoding".into());
    }
    if let Some(backing) = backing.as_deref_mut() {
        backing.metadata_charge = Some(super::memory36::MetadataCharge::reserve(
            &backing.store.resident_budget(),
            backed_metadata_bytes(&index)?,
            backing.check,
        )?);
    }
    // Interleaved native color reads give each eligible range its own window.
    // Sharing one here would refill it on every channel switch.
    let source = row_stream::Source::new(reader, index.file_bytes)?;
    if index.color_channels == 1 {
        if require_peerbrush_source {
            return Err("Unpaired recovery requires an RGB PeerBrush source; the file is retained".into());
        }
        return resident_grayscale(&source, &index, before, control).map(|doc| (doc, None));
    }
    // The legacy FNV binding belongs to supplementary editable sources.
    // Conventional files still receive both complete SHA-256 observations.
    let (section_hash, composite_hash) = if has_supplementary {
        (
            hash_range(&source, index.layer_and_mask, control)?,
            hash_range(&source, index.composite.span, control)?,
        )
    } else {
        (0, 0)
    };
    let layout = if index.header.format == structural::Format::Psb {
        Layout::Psb
    } else {
        Layout::Psd
    };
    let (w, h, depth) = (
        index.header.width,
        index.header.height,
        index.header.depth,
    );
    let file_info_oversized = file_info::indexed_length(&index)? > file_info::MAX_BYTES as u64;
    let mut input = Input {
        source,
        index,
        embedded: None,
        section_hash,
        backing,
    };
    let (resources, section) = input.metadata(layout, control, file_info_oversized)?;
    #[cfg(test)]
    observe_open_phase("sections_read", resources.len() + section.len());
    control.check()?;
    let (merged, merged_white_valid) = if backed {
        (Raster::new_depth(w, h, depth), false)
    } else {
        resident_composite(&input.source, &input.index, control)?
    };
    #[cfg(test)]
    if !backed {
        observe_open_phase("composite_decoded", 0);
    }
    let document = decode_parts(
        &resources,
        &section,
        w,
        h,
        depth,
        merged,
        merged_white_valid,
        composite_hash,
        layout,
        control,
        Some(&mut input),
        require_peerbrush_source,
        file_info_oversized,
    )?;
    if source_hash(&input.source, input.index.file_bytes, control)? != before {
        return Err("Photoshop source bytes changed during decoding".into());
    }
    control.check()?;
    let saved_composite = expected.is_some()
        && indexed_saved_composite(&input.source, &input.index, &|| control.check())?;
    control.check()?;
    let index = input.index;
    let proof = (saved_composite
        && !document.read_only
        && document.icc_profile.is_none()
        && !has_supplementary)
    .then(|| ConventionalComposite {
        header: index.header,
        encoding: index.composite,
        file_bytes: index.file_bytes,
        has_alpha: index.header.channels == 4 && index.merged_transparency,
    });
    Ok((document, proof))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::{Cell, RefCell};
    use std::rc::Rc;

    const GRAY_RESOLUTION: [u8; 16] = [1, 44, 128, 0, 0, 1, 0, 1, 1, 145, 64, 0, 0, 2, 0, 2];

    #[test]
    fn version_info_cannot_hide_a_conflicting_saved_composite_declaration() {
        let resource = |prefix: &[u8]| {
            let mut bytes = b"8BIM".to_vec();
            u16b(&mut bytes, VERSION_INFO_RESOURCE_ID);
            bytes.extend([0, 0]);
            block(&mut bytes, prefix);
            if prefix.len() % 2 != 0 {
                bytes.push(0);
            }
            bytes
        };
        let present = resource(&[0, 0, 0, 1, 1]);
        let absent = resource(&[0, 0, 0, 1, 0]);
        for depth in [8, 16] {
            let mut doc = Document::new_depth(2, 1, depth).unwrap();
            doc.layers[0].pixels.set16(0, 0, [12347, 23459, 34571, 65535]);
            for layout in [Layout::Psd, Layout::Psb] {
                let original = encode_layout(&doc, layout).unwrap();
                let mut cursor = Cursor::new(&original);
                cursor.bytes(26).unwrap();
                cursor.block().unwrap();
                let start = cursor.pos;
                cursor.block().unwrap();
                let with_resources = |resources: &[u8]| {
                    let mut encoded = original[..start].to_vec();
                    block(&mut encoded, resources);
                    encoded.extend_from_slice(&original[cursor.pos..]);
                    encoded
                };
                let valid = with_resources(&present);
                assert_eq!(decode_slice(&valid, &Default::default()).unwrap().layers[0].pixels, doc.layers[0].pixels);
                for (resources, reason, count) in [
                    ([absent.clone(), present.repeat(3)].concat(), "Duplicate", 2),
                    ([present.clone(), absent.clone()].concat(), "Duplicate", 2),
                    (resource(&[0, 0, 0, 1, 2]), "Invalid", 1),
                    (resource(&[0, 0, 0, 1]), "Invalid", 1),
                ] {
                    let encoded = with_resources(&resources);
                    let mut reader = std::io::Cursor::new(&encoded);
                    let index = structural::index_for_decode(&mut reader, || Ok(())).unwrap();
                    assert_eq!(index.resources.iter().filter(|r| r.id == VERSION_INFO_RESOURCE_ID).count(), count);
                    let source = row_stream::Source::new(reader, encoded.len() as u64).unwrap();
                    for error in [
                        decode_slice(&encoded, &Default::default()).unwrap_err(),
                        decode_reader(&mut std::io::Cursor::new(&encoded), &Default::default()).unwrap_err(),
                        preview_profile(&source, &index, &Default::default(), &|| Ok(())).unwrap_err(),
                    ] {
                        assert!(error.contains(reason) && error.contains("Version Info"), "{error}");
                    }
                }
            }
        }
    }

    fn grayscale_fixture(layout: Layout, depth: u16, width: u32, alpha: bool, profile: Option<&[u8]>) -> Vec<u8> {
        let mut out = b"8BPS".to_vec();
        u16b(&mut out, if layout.large() { 2 } else { 1 });
        out.extend([0; 6]);
        u16b(&mut out, if alpha { 2 } else { 1 });
        u32b(&mut out, 1);
        u32b(&mut out, width);
        u16b(&mut out, depth);
        u16b(&mut out, 1);
        u32b(&mut out, 0);
        let mut resources = resolution_resource(&GRAY_RESOLUTION).to_vec();
        resources.extend_from_slice(&print_scale_resource(&TEST_PRINT_SCALE));
        if let Some(profile) = profile {
            resources.extend(b"8BIM");
            u16b(&mut resources, 1039);
            resources.extend([0; 2]);
            u32b(&mut resources, profile.len() as u32);
            resources.extend(profile);
            if profile.len() % 2 != 0 { resources.push(0); }
        }
        block(&mut out, &resources);
        let mut layer_section = Vec::new();
        if alpha {
            layout.write_length(&mut layer_section, 0).unwrap();
            u32b(&mut layer_section, 0);
            let key = if depth == 16 { b"Mt16" } else { b"Mtrn" };
            layer_section.extend(b"8BIM");
            layer_section.extend(key);
            layout.write_length_for_tag(&mut layer_section, key, 0).unwrap();
        }
        layout.write_length(&mut out, layer_section.len() as u64).unwrap();
        out.extend(layer_section);
        u16b(&mut out, 0);
        let (max, exact) = if depth == 8 { (255, 47) } else { (65535, 12347) };
        let gray = if alpha { [max, max / 2, exact] } else { [0, max / 2, exact] };
        let opacity = [0, max / 2 + 1, max];
        for plane in [gray, opacity].into_iter().take(if alpha { 2 } else { 1 }) {
            for x in 0..width as usize {
                let value = plane[x % 3];
                if depth == 8 { out.push(value as u8); } else { u16b(&mut out, value); }
            }
        }
        out
    }

    #[test]
    fn grayscale_psd_and_psb_show_the_protected_composite_at_original_depth() {
        for layout in [Layout::Psd, Layout::Psb] {
            for depth in [8, 16] {
                let max = 65535;
                let exact = if depth == 8 { 47 * 257 } else { 12347 };
                for alpha in [false, true] {
                    let bytes = grayscale_fixture(layout, depth, 3, alpha, None);
                    for doc in [super::super::decode(&bytes).unwrap(), decode(&mut std::io::Cursor::new(bytes.clone()), &Default::default()).unwrap()] {
                        assert!(doc.read_only && doc.warnings.iter().any(|warning| warning.contains("grayscale")));
                        assert_eq!(doc.bit_depth, depth);
                        let pixels = &doc.layers[0].pixels;
                        assert_eq!(pixels.get16(0, 0), if alpha { [0; 4] } else { [0, 0, 0, max] });
                        let gray = if depth == 8 { 127 * 257 } else { 32767 };
                        assert_eq!(pixels.get16(1, 0), if alpha {
                            [0, 0, 0, if depth == 8 { 128 * 257 } else { 32768 }]
                        } else { [gray, gray, gray, max] });
                        assert_eq!(pixels.get16(2, 0), [exact, exact, exact, max]);
                    }
                    assert!(super::super::decode(&bytes[..bytes.len() - 1]).is_err());
                }
                let malformed = grayscale_fixture(layout, depth, 3, false, Some(b"GRAY"));
                assert!(super::super::decode(&malformed).unwrap_err().contains("ICC"));
                let mismatched = grayscale_fixture(layout, depth, 3, false, Some(crate::color_profile::srgb_profile()));
                assert!(super::super::decode(&mismatched).unwrap_err().contains("monochrome profile"));
            }
        }
    }

    #[test]
    fn wide_grayscale_psb16_stages_only_a_protected_native_preview() {
        use crate::engine::native::{worker::tests::Fixture, NativeDocument};
        let fixture = Fixture::new();
        let (runtime, _connection) = fixture.runtime_with_budgets(192 * 1024 * 1024, 8 * 1024 * 1024);
        let session = runtime.begin().unwrap();
        let fragment = session.fresh_fragment().unwrap();
        let profile = moxcms::ColorProfile::new_gray_with_gamma(1.).encode().unwrap();
        let small = grayscale_fixture(Layout::Psb, 16, 3, true, Some(&profile));
        let mut invalid_resolution = small.clone();
        let tag = invalid_resolution.windows(6).position(|window| window == b"8BIM\x03\xed").unwrap();
        invalid_resolution[tag + 17] = 3;
        let invalid = super::super::decode(&invalid_resolution).unwrap();
        assert!(invalid.read_only && invalid.psd_resolution.is_none());
        assert_eq!(invalid.psd_print_scale, Some(TEST_PRINT_SCALE));
        assert!(invalid.warnings.iter().any(|warning| warning.contains("print resolution")));
        let resident = super::super::decode(&small).unwrap();
        assert!(resident.read_only && resident.srgb_tagged && resident.icc_profile.is_none());
        assert_eq!(resident.psd_resolution, Some(GRAY_RESOLUTION));
        assert_eq!(resident.psd_print_scale, Some(TEST_PRINT_SCALE));
        let expected = resident.layers[0].pixels.get16(2, 0);
        let mut engine = crate::engine::Engine::new();
        engine.resident_state_mut().unwrap().document = resident;
        let engine = Arc::new(std::sync::Mutex::new(engine));
        crate::server::compatible_copy(&engine, "human", Some(0)).unwrap();
        {
            let engine = engine.lock().unwrap();
            let copy = &engine.resident_state().unwrap().document;
            assert_eq!(copy.psd_resolution, Some(GRAY_RESOLUTION));
            assert_eq!(copy.psd_print_scale, Some(TEST_PRINT_SCALE));
            assert_eq!(copy.layers[0].pixels.get16(2, 0), expected);
        }
        let linear = 12347. / 65535.;
        let encoded = ((1.055 * f64::powf(linear, 1. / 2.4) - 0.055) * 65535.).round() as u16;
        assert!(expected[..3].iter().all(|value| value.abs_diff(encoded) <= 12));
        assert_eq!(expected[3], 65535);
        let bytes = grayscale_fixture(Layout::Psb, 16, 9000, true, Some(&profile));
        assert!(super::super::decode(&bytes).is_err());
        let path = fixture.path().join("Gray.psb");
        std::fs::write(&path, &bytes).unwrap();
        let mut file = crate::file_safety::open_native_read(&path).unwrap();
        let check = || Ok(());
        let version = crate::file_safety::native_source_version(&path, &mut file, &mut [0; 65536], &check).unwrap();
        let preview = decode_protected(&mut file, version, &session, fragment.store().clone(),
            &Default::default(), &check).unwrap();
        let region = crate::raster::backed::Region::new([8998, 0, 9000, 1]).unwrap();
        let pixels = preview.surface.prepare_region(region, &mut || Ok(())).unwrap();
        assert_eq!(pixels.pixel16(8998, 0).unwrap(), [0, 0, 0, 32768]);
        assert_eq!(pixels.pixel16(8999, 0).unwrap(), expected);
        drop(pixels);
        let document = NativeDocument::new_protected_preview(preview.surface, "Gray", preview.notice.unwrap(),
            preview.profile, preview.srgb_tagged, preview.document_resources, preview.file_info,
            &session.resident(), &session.disk(), &check).unwrap();
        assert!(document.metadata().srgb_tagged() && document.metadata().profile().is_none());
        assert_eq!(document.metadata().psd_resources(), DocumentResources {
            resolution: Some(GRAY_RESOLUTION), scale: Some(TEST_PRINT_SCALE),
            pixel_aspect: None,
            grid: None,
            layer_id_seed: None,
            print_flags: None,
            print_flags_info: None,
            copyright: None,
            global_angle: None,
            global_altitude: None,
            intentionally_untagged: false,
        });
        assert!(document.capture_edit().is_err());
        assert_eq!(std::fs::read(&path).unwrap(), bytes);

        let index = structural::index_protected(&mut std::io::Cursor::new(&bytes), &check).unwrap();
        let data = index.composite.payload.offset as usize;
        let mut zip = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::fast());
        zip.write_all(&bytes[data..]).unwrap();
        let mut compressed = bytes[..data - 2].to_vec();
        u16b(&mut compressed, 2);
        compressed.extend(zip.finish().unwrap());
        let zip_path = fixture.path().join("Gray-ZIP.psb");
        std::fs::write(&zip_path, compressed).unwrap();
        let mut file = crate::file_safety::open_native_read(&zip_path).unwrap();
        let version = crate::file_safety::native_source_version(&zip_path, &mut file, &mut [0; 65536], &check).unwrap();
        let zip_fragment = session.fresh_fragment().unwrap();
        let preview = decode_protected(&mut file, version, &session, zip_fragment.store().clone(),
            &Default::default(), &check).unwrap();
        let pixels = preview.surface.prepare_region(region, &mut || Ok(())).unwrap();
        assert_eq!(pixels.pixel16(8998, 0).unwrap(), [0, 0, 0, 32768]);
        assert_eq!(pixels.pixel16(8999, 0).unwrap(), expected);
        drop((pixels, preview));
        session.abort_fragment(zip_fragment, &mut || session.cleanup_check()).unwrap();
    }

    #[test]
    fn native_protected_report_keeps_original_layer_records_and_saved_samples() {
        use crate::engine::{native::{worker::tests::Fixture, NativeDocument}, view::DocumentView, Engine};
        for (depth, layout) in [(8, Layout::Psd), (16, Layout::Psb)] {
            let fixture = Fixture::new();
            let (runtime, connection) = fixture.runtime();
            let session = runtime.begin().unwrap();
            let fragment = session.fresh_fragment().unwrap();
            let mut original = sample(depth);
            original.layers[0].name = "Lumière".into();
            let original_layer = original.layers[0].id.clone();
            let expected_sample = original.layers[0].pixels.get16(0, 0);
            crate::grouping::create(&mut original, std::slice::from_ref(&original_layer), "Original folder").unwrap();
            original.layers.iter_mut().find(|layer| layer.id == original_layer).unwrap().locked = true;
            let encoded = encode_layout(&original, layout).unwrap();
            let mut bytes = without_sources(&encoded);
            let mut cursor = std::io::Cursor::new(&bytes);
            let index = structural::index(&mut cursor, structural::Limits::native_standard(), || Ok(())).unwrap();
            let child = index.layers.iter().find(|layer| layer.bounds == [0, 0, 259, 257]
                && !layer.tags.iter().any(|tag| tag.key == *b"lsct")).unwrap();
            let tag = child.tags.iter().find(|tag| tag.key == *b"lspf").unwrap();
            let key = tag.data.offset as usize - 8;
            // An unsupported vector-mask payload forces saved-composite protection.
            bytes[key..key + 4].copy_from_slice(b"vmsk");
            assert!(super::super::decode(&bytes).unwrap().read_only);
            let path = fixture.path().join("protected.psd");
            std::fs::write(&path, &bytes).unwrap();
            let mut file = crate::file_safety::open_native_read(&path).unwrap();
            let check = || Ok(());
            let version = crate::file_safety::native_source_version(&path, &mut file, &mut [0; 65536], &check).unwrap();
            let preview = decode_protected(&mut file, version, &session, fragment.store().clone(),
                &Default::default(), &check).unwrap();
            let region = crate::raster::backed::Region::new([0, 0, 1, 1]).unwrap();
            assert_eq!(preview.surface.prepare_region(region, &mut || Ok(())).unwrap().pixel16(0, 0).unwrap(), expected_sample);
            let native = NativeDocument::new_protected_preview(preview.surface, "protected", "unsupported vector mask",
                preview.profile, preview.srgb_tagged, preview.document_resources, preview.file_info,
                &session.resident(), &session.disk(), &check).unwrap()
                .with_protected_structure(preview.structure).unwrap();
            let layers = DocumentView::from(&native).source_structure().unwrap();
            assert_eq!(layers.len(), 2);
            assert_eq!(layers[0].name, "Original folder");
            assert_eq!(layers[1].name, "Lumière");
            assert_eq!(layers[1].parent_index, Some(layers[0].source_index));
            assert_eq!(layers[1].bounds, [0, 0, 257, 259]);
            assert!(layers[1].has_mask && layers[1].tags.iter().any(|tag| tag == "vmsk"));
            assert!(layers[1].warnings.iter().any(|warning| warning.contains("Unsupported Photoshop vector mask")));
            let warnings = serde_json::json!(layers[1].warnings);
            assert!(native.capture_edit().is_err());
            let mut engine = Engine::from_native(native, 4).unwrap();
            let report = engine.capture_state().unwrap().project().unwrap();
            assert_eq!(report["document"]["source_structure"][1]["name"], "Lumière");
            assert_eq!(report["document"]["source_structure"][1]["warnings"], warnings);
            assert_eq!(report["document"]["bit_depth"], depth);
            assert_eq!(report["layers"].as_array().unwrap().len(), 1);
            drop((engine, report, fragment, session, connection));
        }
    }

    #[test]
    fn backed_raster_open_admits_metadata_separately_from_skipped_file_payloads() {
        use crate::{
            engine::native::worker::tests::Fixture,
            large_document::{DiskBudget, Limits, ResidentBudget, Store},
        };
        let fixture = Fixture::new();
        let path = fixture.path().join("Standard.psd");
        let document = sample(8);
        let encoded = encode(&document).unwrap();
        let end = 34 + u32::from_be_bytes(encoded[30..34].try_into().unwrap()) as usize;
        let skipped = RESIDENT_FILE_BYTES as u32;
        let mut standard = encoded[..30].to_vec();
        u32b(&mut standard, 12 + skipped);
        standard.extend_from_slice(b"8BIM");
        u16b(&mut standard, 4100);
        standard.extend_from_slice(&[0; 2]);
        u32b(&mut standard, skipped);
        // Use a hole for the ignored resource so the fixture never owns a
        // file-sized byte buffer. Only the small standard raster is staged.
        let mut output = std::fs::File::create(&path).unwrap();
        output.write_all(&standard).unwrap();
        output.seek(SeekFrom::Current(i64::from(skipped))).unwrap();
        output.write_all(&encoded[end..]).unwrap();
        drop(output);

        let pool = ResidentBudget::new(16 * 1024 * 1024).unwrap();
        let store = Store::create_with_disk_budget(
            &fixture.path().join("decoded"),
            pool.clone(),
            DiskBudget::new(32 * 1024 * 1024, 4).unwrap(),
            Limits {
                disk_bytes: 32 * 1024 * 1024,
                max_blocks: 64,
                max_resident_blocks: 8,
                manifest_bytes: 1024 * 1024,
            },
        ).unwrap();
        let mut file = crate::file_safety::open_native_read(&path).unwrap();
        let check = || Ok(());
        let version = crate::file_safety::native_source_version(
            &path, &mut file, &mut [0; 65536], &check,
        ).unwrap();
        assert!(version.bytes > RESIDENT_FILE_BYTES);
        assert!(2 * version.bytes as usize + 8 * 1024 * 1024 > pool.limit());
        assert!(decode(&mut file, &Default::default()).unwrap_err().contains("256 MiB"));
        let decoded = decode_backed(
            &mut file, &Default::default(), version, store, &check,
        ).unwrap();
        let layer = &decoded.document.layers[0];
        let surface = &decoded.color[&layer.id];
        let region = crate::raster::backed::Region::new([256, 258, 257, 259]).unwrap();
        assert_eq!(
            surface.prepare_region(region, &mut || Ok(())).unwrap().pixel16(256, 258).unwrap(),
            document.layers[0].pixels.get16(256, 258),
        );
        assert_eq!(layer.pixels.bytes(), 0);
    }

    fn sample(depth: u16) -> Document {
        let mut doc = Document::new_depth(257, 259, depth).unwrap();
        for y in 0..doc.height {
            for x in 0..doc.width {
                let v = (12347u32 + x * 173 + y * 211) as u16;
                doc.layers[0].pixels.set16(
                    x as i32,
                    y as i32,
                    [v, v.wrapping_mul(3), v.wrapping_mul(7), 65535],
                );
            }
        }
        doc
    }

    fn without_sources(bytes: &[u8]) -> Vec<u8> {
        let at = 30 + u32::from_be_bytes(bytes[26..30].try_into().unwrap()) as usize;
        let size = u32::from_be_bytes(bytes[at..at + 4].try_into().unwrap()) as usize;
        let mut out = bytes[..at].to_vec();
        u32b(&mut out, 0);
        out.extend_from_slice(&bytes[at + 4 + size..]);
        out
    }

    fn compare(actual: &Document, expected: &Document) {
        assert_eq!(
            (
                actual.width,
                actual.height,
                actual.bit_depth,
                actual.read_only
            ),
            (
                expected.width,
                expected.height,
                expected.bit_depth,
                expected.read_only
            )
        );
        assert_eq!(actual.warnings, expected.warnings);
        assert_eq!(
            serde_json::to_value(&actual.source_structure).unwrap(),
            serde_json::to_value(&expected.source_structure).unwrap()
        );
        assert_eq!(
            (
                &actual.icc_profile,
                actual.srgb_tagged,
                &actual.psd_global_mask
            ),
            (
                &expected.icc_profile,
                expected.srgb_tagged,
                &expected.psd_global_mask
            )
        );
        assert_eq!(actual.layers.len(), expected.layers.len());
        for (a, b) in actual.layers.iter().zip(&expected.layers) {
            assert_eq!(
                (
                    &a.name,
                    &a.kind,
                    a.x,
                    a.y,
                    a.visible,
                    a.locked,
                    a.opacity,
                    &a.blend,
                    &a.psd_metadata
                ),
                (
                    &b.name,
                    &b.kind,
                    b.x,
                    b.y,
                    b.visible,
                    b.locked,
                    b.opacity,
                    &b.blend,
                    &b.psd_metadata
                )
            );
            assert_eq!(
                serde_json::to_value(&a.pixels).unwrap(),
                serde_json::to_value(&b.pixels).unwrap()
            );
            assert_eq!(a.mask.is_some(), b.mask.is_some());
            if let (Some(a), Some(b)) = (&a.mask, &b.mask) {
                assert_eq!(
                    (a.enabled, a.linked, a.steps.len()),
                    (b.enabled, b.linked, b.steps.len())
                );
                for (a, b) in a.steps.iter().zip(&b.steps) {
                    assert_eq!(
                        (&a.kind, a.enabled, a.weight, a.value),
                        (&b.kind, b.enabled, b.weight, b.value)
                    );
                    assert_eq!(
                        serde_json::to_value(&a.pixels).unwrap(),
                        serde_json::to_value(&b.pixels).unwrap()
                    );
                }
            }
        }
    }

    #[test]
    fn indexed_standard_import_matches_borrowed_native_psd_psb_and_private_sources() {
        for depth in [8, 16] {
            for layout in [Layout::Psd, Layout::Psb] {
                let doc = sample(depth);
                let encoded = encode_layout(&doc, layout).unwrap();
                for private in [false, true] {
                    let bytes = if private {
                        encoded.clone()
                    } else {
                        without_sources(&encoded)
                    };
                    let legacy = super::super::decode(&bytes).unwrap();
                    let streamed = decode_reader(
                        &mut std::io::Cursor::new(&bytes),
                        &crate::loading::Control::default(),
                    )
                    .unwrap();
                    compare(&streamed, &legacy);
                    assert!(!streamed.read_only);
                    if private {
                        assert_eq!(
                            serde_json::to_value(&streamed).unwrap(),
                            serde_json::to_value(&doc).unwrap()
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn unsupported_global_layer_and_duplicate_sources_keep_the_same_protected_preview() {
        for format in [structural::Format::Psd, structural::Format::Psb] {
            for depth in [8, 16] {
                for rle in [false, true] {
                    let bytes = structural::tests::fixture(format, depth, rle);
                    let legacy = super::super::decode(&bytes).unwrap();
                    let streamed = decode_reader(
                        &mut std::io::Cursor::new(&bytes),
                        &crate::loading::Control::default(),
                    )
                    .unwrap();
                    assert!(streamed.read_only);
                    compare(&streamed, &legacy);
                }
            }
        }
    }

    struct Observed {
        bytes: Rc<RefCell<Vec<u8>>>,
        position: u64,
        maximum: Rc<Cell<usize>>,
        mutation: Option<(u64, usize)>,
    }
    impl Read for Observed {
        fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
            self.maximum.set(self.maximum.get().max(out.len()));
            if let Some((trigger, at)) = self.mutation {
                if self.position == trigger {
                    self.bytes.borrow_mut()[at] ^= 1;
                    self.mutation = None;
                }
            }
            let data = self.bytes.borrow();
            let start = usize::try_from(self.position).unwrap();
            let n = out.len().min(data.len().saturating_sub(start));
            out[..n].copy_from_slice(&data[start..start + n]);
            self.position += n as u64;
            Ok(n)
        }
    }
    impl Seek for Observed {
        fn seek(&mut self, from: SeekFrom) -> std::io::Result<u64> {
            let value = match from {
                SeekFrom::Start(v) => v as i128,
                SeekFrom::Current(v) => self.position as i128 + v as i128,
                SeekFrom::End(v) => self.bytes.borrow().len() as i128 + v as i128,
            };
            if !(0..=u64::MAX as i128).contains(&value) {
                return Err(std::io::Error::other("Invalid fixture seek"));
            }
            self.position = value as u64;
            Ok(self.position)
        }
    }

    #[test]
    fn encoded_bulk_and_private_resource_stay_out_of_metadata_and_reads_remain_bounded() {
        for depth in [8, 16] {
            let bytes = encode_layout(&sample(depth), Layout::Psb).unwrap();
            let maximum = Rc::new(Cell::new(0));
            let shared = Rc::new(RefCell::new(bytes));
            let mut reader = Observed {
                bytes: shared.clone(),
                position: 0,
                maximum: maximum.clone(),
                mutation: None,
            };
            let index =
                structural::index(&mut reader, structural::Limits::current(), || Ok(())).unwrap();
            let source = row_stream::Source::new(&mut reader, index.file_bytes).unwrap();
            let mut input = Input {
                source,
                index,
                embedded: None,
                section_hash: 0,
                backing: None,
            };
            let (resources, section) = input
                .metadata(Layout::Psb, &crate::loading::Control::default(), false)
                .unwrap();
            assert!(resources.len() < 128 && section.len() < 4096);
            assert!(input.embedded_len() > 4096);
            assert!(section.len() as u64 * 100 < input.index.layer_and_mask.bytes);
            drop(input);
            let actual = decode_reader(&mut reader, &crate::loading::Control::default()).unwrap();
            assert!(!actual.read_only);
            assert!(maximum.get() <= 65536);
        }
    }

    #[test]
    fn same_length_source_mutation_during_indexing_is_refused_before_publication() {
        let mut bytes = encode(&sample(8)).unwrap();
        // Insert an ignored resource. Its payload has no influence on pixels,
        // metadata or standard FNV hashes, but remains bound by the source SHA.
        let resources = u32::from_be_bytes(bytes[30..34].try_into().unwrap()) as usize;
        let mut resource = b"8BIM".to_vec();
        u16b(&mut resource, 1999);
        resource.extend_from_slice(&[0, 0]);
        block(&mut resource, &[7, 11]);
        let at = 34 + resources;
        bytes.splice(at..at, resource.iter().copied());
        bytes[30..34].copy_from_slice(&((resources + resource.len()) as u32).to_be_bytes());
        let maximum = Rc::new(Cell::new(0));
        let shared = Rc::new(RefCell::new(bytes));
        let mut reader = Observed {
            bytes: shared,
            position: 0,
            maximum,
            mutation: Some((at as u64, at + resource.len() - 1)),
        };
        let error = decode_reader(&mut reader, &crate::loading::Control::default()).unwrap_err();
        assert!(error.contains("source bytes changed"), "{error}");
    }

    #[test]
    fn duplicate_native_layer_info_keeps_protection_without_amplifying_active_metadata() {
        let original = without_sources(&encode_psb(&sample(16)).unwrap());
        let index = structural::index(
            &mut std::io::Cursor::new(&original),
            structural::Limits::current(),
            || Ok(()),
        )
        .unwrap();
        let first = index
            .global_tags
            .iter()
            .find(|tag| tag.key == *b"Lr16")
            .unwrap();
        let tag_start = first.data.offset - 16;
        let mut section =
            original[index.layer_and_mask.offset as usize..tag_start as usize].to_vec();
        for _ in 0..64 {
            section.extend_from_slice(b"8BIMLr16");
            section.extend_from_slice(&2u64.to_be_bytes());
            section.extend_from_slice(&[0; 4]);
        }
        section.extend_from_slice(
            &original[tag_start as usize..index.layer_and_mask.end().unwrap() as usize],
        );
        let mut bytes = original[..index.layer_and_mask.offset as usize - 8].to_vec();
        Layout::Psb
            .write_length(&mut bytes, section.len() as u64)
            .unwrap();
        bytes.extend(section);
        bytes.extend_from_slice(&original[index.composite.span.offset as usize..]);
        let mut reader = std::io::Cursor::new(&bytes);
        let index =
            structural::index(&mut reader, structural::Limits::current(), || Ok(())).unwrap();
        let source = row_stream::Source::new(&mut reader, index.file_bytes).unwrap();
        let mut input = Input {
            source,
            index,
            embedded: None,
            section_hash: 0,
            backing: None,
        };
        let (_, compact) = input.metadata(Layout::Psb, &Default::default(), false).unwrap();
        let tags = global_blocks(&compact, Layout::Psb).unwrap().2;
        assert!(tags
            .iter()
            .take(64)
            .all(|(key, payload)| *key == b"Lr16" && payload.len() == 2));
        assert!(compact.len() < 4096);
        drop(input);
        let expected = super::super::decode(&bytes).unwrap();
        let actual = decode_reader(&mut reader, &Default::default()).unwrap();
        assert!(actual.read_only);
        compare(&actual, &expected);
    }

    #[test]
    fn duplicate_native_channel_ids_protect_the_saved_composite_in_both_readers() {
        for depth in [8, 16] {
            for layout in [Layout::Psd, Layout::Psb] {
                let mut bytes = without_sources(&encode_layout(&sample(depth), layout).unwrap());
                let index = structural::index(
                    &mut std::io::Cursor::new(&bytes),
                    structural::Limits::current(),
                    || Ok(()),
                )
                .unwrap();
                let first = index.layers[0].record.offset as usize + 18;
                let second = first + if layout.large() { 10 } else { 6 };
                let duplicate = bytes[first..first + 2].to_vec();
                bytes[second..second + 2].copy_from_slice(&duplicate);
                let expected = super::super::decode(&bytes).unwrap();
                let actual =
                    decode_reader(&mut std::io::Cursor::new(&bytes), &Default::default()).unwrap();
                assert!(actual.read_only);
                assert!(actual
                    .warnings
                    .iter()
                    .any(|w| w.contains("Duplicate Photoshop layer channel")));
                compare(&actual, &expected);
            }
        }
    }

    #[test]
    fn formerly_ignored_resource_and_protected_layer_tag_counts_keep_legacy_behavior() {
        for bytes in [
            structural::tests::many_resources_fixture(),
            structural::tests::many_layer_tags_fixture(),
        ] {
            let expected = super::super::decode(&bytes).unwrap();
            let actual =
                decode_reader(&mut std::io::Cursor::new(&bytes), &Default::default()).unwrap();
            assert!(actual.read_only);
            compare(&actual, &expected);
        }
    }

    #[test]
    fn documented_odd_raw_layer_rows_and_nonzero_layer_info_padding_keep_native_samples() {
        for format in [structural::Format::Psd, structural::Format::Psb] {
            for padded in [false, true] {
                let bytes = structural::tests::odd_raw_fixture(format, padded);
                let actual =
                    decode_reader(&mut std::io::Cursor::new(&bytes), &Default::default()).unwrap();
                assert!(!actual.read_only);
                for y in 0..3i32 {
                    for x in 0..3i32 {
                        assert_eq!(
                            actual.layers[0].pixels.get(x, y),
                            [0, 1, 2, 3].map(|c| if c == 3 {
                                255
                            } else {
                                c * 70 + x as u8 * 3 + y as u8 * 11 + 7
                            })
                        );
                    }
                }
                if !padded {
                    compare(&actual, &super::super::decode(&bytes).unwrap());
                }
            }
        }
    }

    #[test]
    fn protected_adjustment_records_keep_undefined_channel_data_out_of_editable_decoding() {
        for format in [structural::Format::Psd, structural::Format::Psb] {
            for depth in [8, 16] {
                let mut bytes = structural::tests::fixture(format, depth, false);
                let index = structural::index(
                    &mut std::io::Cursor::new(&bytes),
                    structural::Limits::current(),
                    || Ok(()),
                )
                .unwrap();
                let record = &index.layers[0];
                bytes[record.record.offset as usize..record.record.offset as usize + 16].fill(0);
                let tag = record.tags.iter().find(|tag| tag.key == *b"abcd").unwrap();
                bytes[tag.data.offset as usize - 8..tag.data.offset as usize - 4]
                    .copy_from_slice(b"SoCo");
                let expected = super::super::decode(&bytes).unwrap();
                let actual =
                    decode_reader(&mut std::io::Cursor::new(&bytes), &Default::default()).unwrap();
                assert!(actual.read_only);
                compare(&actual, &expected);
            }
        }
    }
}
