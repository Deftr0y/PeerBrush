//! Encoded-source streaming at the current admission and decoded-raster limits.
//!
//! Metadata is a compact parser view, never the bytes used to bind private
//! definitions. The original source stays borrowed until every stream and the
//! final whole-source observation complete. Decoded Raster residency is unchanged.
use super::{row_stream, structural, *};
use std::sync::Arc;

struct Input<'a, R> {
    source: row_stream::Source<&'a mut R>,
    index: structural::Index,
    embedded: Option<structural::Span>,
    section_hash: u64,
}

fn append<R: Read + Seek>(
    source: &row_stream::Source<R>,
    span: structural::Span,
    out: &mut Vec<u8>,
    control: &crate::loading::Control,
) -> Result<(), String> {
    // Metadata cannot exceed the existing whole-file admission. No new large
    // source/metadata admission is implied by the structural index's u64 spans.
    let count = usize::try_from(span.bytes).map_err(|_| "Photoshop metadata size overflow")?;
    let end = out
        .len()
        .checked_add(count)
        .filter(|n| *n <= 256 * 1024 * 1024)
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
    let check = || control.check();
    let mut input = source.range(span, &check)?;
    let mut bytes = [0; 65536];
    let mut result = 0xcbf29ce484222325;
    loop {
        let n = input.read(&mut bytes).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        hash_part(&mut result, &bytes[..n]);
    }
    control.check()?;
    Ok(result)
}

fn source_hash<R: Read + Seek>(
    source: &row_stream::Source<R>,
    count: u64,
    control: &crate::loading::Control,
) -> Result<[u8; 32], String> {
    source.unchanged_length()?;
    let check = || control.check();
    let mut input = source.range(
        structural::Span {
            offset: 0,
            bytes: count,
        },
        &check,
    )?;
    let mut bytes = [0; 65536];
    let mut hash = ring::digest::Context::new(&ring::digest::SHA256);
    loop {
        let n = input.read(&mut bytes).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        hash.update(&bytes[..n]);
    }
    control.check()?;
    source.unchanged_length()?;
    Ok(crate::file_safety::finish_fingerprint(hash))
}

impl<R: Read + Seek> Input<'_, R> {
    fn metadata(
        &mut self,
        layout: Layout,
        control: &crate::loading::Control,
    ) -> Result<(Vec<u8>, Vec<u8>), String> {
        let mut resources = Vec::new();
        for resource in &self.index.resources {
            control.check()?;
            if resource.peerbrush_source {
                self.embedded = Some(structural::Span {
                    offset: resource.data.offset + 4,
                    bytes: resource.data.bytes - 4,
                });
            }
            // Other resource payloads were ignored by the semantic parser. Keep
            // duplicate source markers and exact ICC/VersionInfo interpretation.
            if !resource.peerbrush_source && ![1039, 1057].contains(&resource.id) {
                continue;
            }
            resources.extend_from_slice(b"8BIM");
            u16b(&mut resources, resource.id);
            resources.extend_from_slice(&[0, 0]);
            let bytes = if resource.peerbrush_source {
                4
            } else if resource.id == 1057 {
                resource.data.bytes.min(5)
            } else {
                if resource.data.bytes > crate::color_profile::MAX_PROFILE_BYTES as u64 {
                    return Err("Photoshop ICC profile exceeds the 4 MiB limit".into());
                }
                resource.data.bytes
            };
            u32b(&mut resources, bytes as u32);
            if resource.peerbrush_source {
                resources.extend_from_slice(b"PBR1");
            } else {
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
            // Inactive compatibility records remain outside this semantic view.
            // Their original bytes still participate in the standard hash.
            let count = self.index.primary_layer_info.bytes.min(2);
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

    fn encoding(&self, record: usize, channel: usize) -> Result<&structural::Encoding, String> {
        self.index
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
    fn write_length_for_tag(
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

impl<R: Read + Seek> IndexedChannels for Input<'_, R> {
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
        let encoding = self.encoding(record, channel)?;
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
        raster: &mut Raster,
        offset: [i32; 2],
        control: &crate::loading::Control,
    ) -> Result<(), String> {
        let encoding = self.encoding(record, channel)?;
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
    control.progress("Reading PSD header", 0, 1)?;
    let bytes = reader.seek(SeekFrom::End(0)).map_err(|e| e.to_string())?;
    if bytes > 256 * 1024 * 1024 {
        return Err("Photoshop file limit is 256 MiB".into());
    }
    let before = source_hash(
        &row_stream::Source::new(&mut *reader, bytes)?,
        bytes,
        control,
    )?;
    let index = structural::index_for_decode(reader, || control.check())?;
    let source = row_stream::Source::new(reader, index.file_bytes)?;
    let section_hash = hash_range(&source, index.layer_and_mask, control)?;
    let composite_hash = hash_range(&source, index.composite.span, control)?;
    let layout = if index.header.format == structural::Format::Psb {
        Layout::Psb
    } else {
        Layout::Psd
    };
    let (w, h, depth, channels) = (
        index.header.width,
        index.header.height,
        index.header.depth,
        index.header.channels,
    );
    let mut input = Input {
        source,
        index,
        embedded: None,
        section_hash,
    };
    let (resources, section) = input.metadata(layout, control)?;
    #[cfg(test)]
    observe_open_phase("sections_read", resources.len() + section.len());
    control.check()?;
    let (merged, merged_white_valid) = {
        let mut merged = Raster::new_depth(w, h, depth);
        let encoding = &input.index.composite;
        let check = || control.check();
        let mut rows = input.source.rows(encoding, &check)?;
        let mut row = vec![0; encoding.row_bytes as usize];
        for c in 0..usize::from(channels) {
            for y in 0..h {
                if y % 16 == 0 {
                    control.progress(
                        "Decoding saved image",
                        c * h as usize + y as usize,
                        usize::from(channels) * h as usize,
                    )?;
                }
                if !rows.read_row(&mut row)? {
                    return Err("Missing saved composite row".into());
                }
                if c != 3 || input.index.merged_transparency {
                    write_channel_row(&mut merged, y, c, c == 0, &row);
                }
            }
        }
        rows.finish()?.check(&input.source, encoding, &check)?;
        restore_saved_composite(merged, control)?
    };
    #[cfg(test)]
    observe_open_phase("composite_decoded", 0);
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
    )?;
    if source_hash(&input.source, input.index.file_bytes, control)? != before {
        return Err("Photoshop source bytes changed during decoding".into());
    }
    control.check()?;
    Ok(document)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::{Cell, RefCell};
    use std::rc::Rc;

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
            };
            let (resources, section) = input
                .metadata(Layout::Psb, &crate::loading::Control::default())
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
        };
        let (_, compact) = input.metadata(Layout::Psb, &Default::default()).unwrap();
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
