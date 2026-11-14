//! Bounded stream indexing, not an authority to restore editable Photoshop sources.
//!
//! Spans refer to the caller's already leased source. The caller must retain its
//! source identity/hash checks before and after using them. Unknown ordinary
//! metadata remains indexed, and requires the existing semantic protection checks.
use std::collections::HashSet;
use std::io::{Read, Seek, SeekFrom};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Format {
    Psd,
    Psb,
}

impl Format {
    fn large(self) -> bool {
        self == Self::Psb
    }

    pub(super) fn rle_count_width(self) -> usize {
        if self.large() { 4 } else { 2 }
    }

    fn tag_large(self, key: &[u8; 4]) -> bool {
        // Match the existing codec's reviewed PSB key-width policy. An 8B64
        // signature alone does not widen an arbitrary key's length field.
        self.large()
            && matches!(
                key,
                b"LMsk"
                    | b"Lr16"
                    | b"Lr32"
                    | b"Layr"
                    | b"Mt16"
                    | b"Mt32"
                    | b"Mtrn"
                    | b"Alph"
                    | b"FMsk"
                    | b"lnk2"
                    | b"FEid"
                    | b"FXid"
                    | b"PxSD"
                    | b"lnk3"
                    | b"lnkE"
                    | b"FELS"
                    | b"pths"
                    | b"extd"
                    | b"extn"
                    | b"cinf"
                    | b"artd"
            )
    }
}

pub(super) fn rle_row_length(word: &[u8], row_bytes: u64) -> Result<u64, String> {
    let length = if word.len() == 4 {
        u64::from(u32::from_be_bytes(word.try_into().unwrap()))
    } else {
        u64::from(u16::from_be_bytes(word.try_into().unwrap()))
    };
    if length > row_bytes.saturating_mul(2).saturating_add(1024) {
        return Err("Packed Photoshop row exceeds transfer budget".into());
    }
    Ok(length)
}

/// The caller has already bounded the count table to its indexed source span.
pub(super) fn sum_rle_table(
    input: &mut impl Read,
    format: Format,
    mut rows: u64,
    row_bytes: u64,
    scratch: &mut [u8],
    check: &mut dyn FnMut() -> Result<(), String>,
) -> Result<u64, String> {
    let width = format.rle_count_width();
    let capacity = scratch.len() / width;
    if capacity == 0 { return Err("Missing Photoshop row table scratch".into()); }
    let mut packed = 0u64;
    while rows != 0 {
        let count = rows.min(capacity as u64) as usize;
        let bytes = &mut scratch[..count * width];
        let mut rest = &mut *bytes;
        while !rest.is_empty() {
            check()?;
            let read = match input.read(rest) {
                Ok(read) => read,
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(_) => return Err("Cannot read Photoshop row table".into()),
            };
            if read == 0 || read > rest.len() {
                return Err("Truncated or invalid Photoshop row table".into());
            }
            rest = &mut rest[read..];
        }
        for word in bytes.chunks_exact(width) {
            packed = packed.checked_add(rle_row_length(word, row_bytes)?)
                .ok_or("Photoshop packed row length overflow")?;
        }
        rows -= count as u64;
    }
    check()?;
    Ok(packed)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct Span {
    pub offset: u64,
    pub bytes: u64,
}

impl Span {
    pub(crate) fn end(self) -> Result<u64, String> {
        self.offset
            .checked_add(self.bytes)
            .ok_or_else(|| "Photoshop span overflow".into())
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct Limits {
    pub file_bytes: u64,
    pub max_edge: u32,
    pub pixels: u64,
    pub layers: usize,
    pub channels: usize,
    pub resources: usize,
    pub tags: usize,
    pub row_bytes: u64,
    pub mask_edge: u32,
    pub mask_pixels8: u64,
    pub mask_pixels16: u64,
    pub coordinate: u32,
}

// The private manifest and either the ICC profile or untagged declaration.
pub(super) const SOURCE36_BASE_RESOURCES: usize = 2;

impl Limits {
    /// Existing admission only. A different index profile cannot change engine admission.
    pub(crate) const fn current() -> Self {
        Self {
            file_bytes: crate::file_safety::RESIDENT_FILE_BYTES,
            max_edge: crate::raster::MAX_EDGE,
            pixels: crate::raster::MAX_PIXELS,
            layers: 200,
            channels: 8,
            resources: 4096,
            tags: 4096,
            row_bytes: crate::raster::MAX_EDGE as u64 * 2,
            mask_edge: 200_000,
            mask_pixels8: 32 * 1024 * 1024,
            mask_pixels16: 128 * 1024 * 1024,
            coordinate: 100_000,
        }
    }

    pub(crate) fn native_standard() -> Self {
        let edge = crate::raster::NATIVE_EDGE;
        let pixels = u64::from(edge) * u64::from(edge);
        Self {
            file_bytes: crate::file_safety::NATIVE_FILE_BYTES,
            max_edge: edge,
            pixels,
            row_bytes: u64::from(edge) * 2,
            mask_pixels8: pixels,
            mask_pixels16: pixels,
            ..Self::current()
        }
    }

    pub(crate) fn source36(file_bytes: u64, chunks: usize) -> Result<Self, String> {
        Ok(Self {
            file_bytes,
            layers: super::standard36::MAX_STANDARD_RECORDS,
            coordinate: 100000 + crate::raster::NATIVE_EDGE,
            tags: chunks
                .checked_add(4096)
                .ok_or("Source36 index tag admission overflow")?,
            resources: SOURCE36_BASE_RESOURCES + super::DocumentResources::SOURCE36_FIXED_IDS.len()
                + super::file_info::IDS.len(),
            ..Self::native_standard()
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct Header {
    pub format: Format,
    pub width: u32,
    pub height: u32,
    pub depth: u16,
    pub channels: u16,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Resource {
    pub id: u16,
    pub name: Span,
    pub data: Span,
    pub peerbrush_source: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Tag {
    pub signature: [u8; 4],
    pub key: [u8; 4],
    pub data: Span,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Compression {
    Raw,
    Rle,
    Zip,
    PredictedZip,
    Empty,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Encoding {
    pub span: Span,
    pub compression: Compression,
    pub rows: u64,
    pub row_bytes: u64,
    pub row_padding: u8,
    pub counts: Option<Span>,
    pub payload: Span,
    pub depth: u16,
    pub format: Format,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Channel {
    pub id: i16,
    pub span: Span,
    // Unknown/advanced mask geometry must be interpreted by the semantic codec,
    // never guessed from the layer's rectangle.
    pub dimensions: Option<[u32; 2]>,
    pub encoding: Option<Encoding>,
    pub encoding_error: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Layer {
    pub record: Span,
    pub bounds: [i32; 4],
    pub blend_mode: [u8; 4],
    pub opacity: u8,
    pub clipping: u8,
    pub flags: u8,
    pub filler: u8,
    pub extra: Span,
    pub mask: Span,
    pub blending_ranges: Span,
    pub name: Span,
    pub tags: Vec<Tag>,
    pub channels: Vec<Channel>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Ambiguity {
    PeerBrushSources,
    ColorProfiles,
    GlobalTag([u8; 4]),
    LayerTag(usize, [u8; 4]),
    LayerChannel(usize, i16),
    PrimaryAndGlobalLayers,
}

#[derive(Clone, Debug)]
pub(crate) struct Index {
    pub file_bytes: u64,
    pub header: Header,
    pub color_channels: u16,
    pub color_data: Span,
    pub resources: Vec<Resource>,
    pub layer_and_mask: Span,
    pub primary_layer_info: Span,
    pub global_mask: Span,
    pub global_tags: Vec<Tag>,
    pub active_layer_info: Span,
    pub merged_transparency: bool,
    pub layers: Vec<Layer>,
    pub composite: Encoding,
    pub ambiguities: Vec<Ambiguity>,
}

struct Scanner<'a, R, C> {
    input: &'a mut R,
    check: &'a mut C,
    limits: Limits,
    tags: usize,
    semantic_only: bool,
}

impl<R: Read + Seek, C: FnMut() -> Result<(), String>> Scanner<'_, R, C> {
    fn position(&mut self) -> Result<u64, String> {
        self.input
            .stream_position()
            .map_err(|_| "Cannot seek Photoshop stream".into())
    }

    fn seek(&mut self, position: u64) -> Result<(), String> {
        (self.check)()?;
        if self
            .input
            .seek(SeekFrom::Start(position))
            .map_err(|_| "Cannot seek Photoshop stream")?
            != position
        {
            return Err("Photoshop stream seek differs".into());
        }
        Ok(())
    }

    fn read<const N: usize>(&mut self, end: u64) -> Result<[u8; N], String> {
        (self.check)()?;
        if self
            .position()?
            .checked_add(N as u64)
            .is_none_or(|n| n > end)
        {
            return Err("Truncated Photoshop structure".into());
        }
        let mut bytes = [0; N];
        self.input
            .read_exact(&mut bytes)
            .map_err(|_| "Cannot read Photoshop stream")?;
        Ok(bytes)
    }

    fn u16(&mut self, end: u64) -> Result<u16, String> {
        Ok(u16::from_be_bytes(self.read(end)?))
    }

    fn u32(&mut self, end: u64) -> Result<u32, String> {
        Ok(u32::from_be_bytes(self.read(end)?))
    }

    fn length(&mut self, end: u64, large: bool) -> Result<u64, String> {
        if large {
            Ok(u64::from_be_bytes(self.read(end)?))
        } else {
            Ok(u64::from(self.u32(end)?))
        }
    }

    fn span(&mut self, bytes: u64, end: u64) -> Result<Span, String> {
        let offset = self.position()?;
        let span = Span { offset, bytes };
        let next = span.end()?;
        if next > end {
            return Err("Truncated Photoshop section".into());
        }
        self.seek(next)?;
        Ok(span)
    }

    fn block(&mut self, end: u64, large: bool) -> Result<Span, String> {
        let bytes = self.length(end, large)?;
        self.span(bytes, end)
    }

    fn pad(&mut self, bytes: u64, alignment: u64, end: u64) -> Result<(), String> {
        self.span((alignment - bytes % alignment) % alignment, end)?;
        Ok(())
    }

    fn zeros(&mut self, end: u64) -> Result<(), String> {
        while self.position()? < end {
            if self.read::<1>(end)? != [0] {
                return Err("Unframed Photoshop trailing data".into());
            }
        }
        Ok(())
    }

    fn bounds(&mut self, end: u64) -> Result<[i32; 4], String> {
        let mut bounds = [0; 4];
        for value in &mut bounds {
            *value = self.u32(end)? as i32;
        }
        Ok(bounds)
    }

    fn dimensions(&self, bounds: [i32; 4]) -> Result<[u32; 2], String> {
        let width = i64::from(bounds[3]) - i64::from(bounds[1]);
        let height = i64::from(bounds[2]) - i64::from(bounds[0]);
        if width < 0
            || height < 0
            || width > i64::from(self.limits.max_edge)
            || height > i64::from(self.limits.max_edge)
            || (width as u64)
                .checked_mul(height as u64)
                .is_none_or(|n| n > self.limits.pixels)
        {
            return Err("Photoshop rectangle exceeds index limits".into());
        }
        Ok([width as u32, height as u32])
    }

    fn tag(&mut self, end: u64, format: Format, alignment: u64) -> Result<Tag, String> {
        if self.tags >= self.limits.tags {
            return Err("Too many Photoshop metadata blocks".into());
        }
        self.tags += 1;
        let signature = self.read(end)?;
        if !matches!(&signature, b"8BIM" | b"8B64") {
            return Err("Invalid Photoshop metadata signature".into());
        }
        let key = self.read(end)?;
        let data = self.block(end, format.tag_large(&key))?;
        self.pad(data.bytes, alignment, end)?;
        Ok(Tag {
            signature,
            key,
            data,
        })
    }

    fn mask_dimensions(&self, bounds: [i32; 4], depth: u16) -> Result<[u32; 2], String> {
        let width = i64::from(bounds[3]) - i64::from(bounds[1]);
        let height = i64::from(bounds[2]) - i64::from(bounds[0]);
        let pixels = if depth == 16 {
            self.limits.mask_pixels16
        } else {
            self.limits.mask_pixels8
        };
        if bounds
            .iter()
            .any(|v| v.unsigned_abs() > self.limits.coordinate)
            || width < 0
            || height < 0
            || width > i64::from(self.limits.mask_edge)
            || height > i64::from(self.limits.mask_edge)
            || (width as u64)
                .checked_mul(height as u64)
                .is_none_or(|n| n > pixels)
        {
            return Err("Photoshop mask exceeds native decode limits".into());
        }
        Ok([width as u32, height as u32])
    }

    fn encoding(
        &mut self,
        span: Span,
        width: u32,
        height: u32,
        channels: u16,
        depth: u16,
        format: Format,
        mask: bool,
        layer_padding: bool,
    ) -> Result<Encoding, String> {
        let end = span.end()?;
        let rows = u64::from(height)
            .checked_mul(u64::from(channels))
            .ok_or("Photoshop row count overflow")?;
        let row_bytes = u64::from(width)
            .checked_mul(u64::from(depth / 8))
            .ok_or("Photoshop row size overflow")?;
        let row_limit = if mask {
            u64::from(self.limits.mask_edge) * u64::from(depth / 8)
        } else {
            self.limits.row_bytes
        };
        if row_bytes > row_limit {
            return Err("Photoshop row exceeds index limits".into());
        }
        self.seek(span.offset)?;
        if span.bytes == 0 && (width == 0 || height == 0) {
            return Ok(Encoding {
                span,
                compression: Compression::Empty,
                rows,
                row_bytes,
                row_padding: 0,
                counts: None,
                payload: span,
                depth,
                format,
            });
        }
        let kind = self.u16(end)?;
        let mut counts = None;
        let compression = match kind {
            0 => Compression::Raw,
            1 => {
                let bytes = rows
                    .checked_mul(format.rle_count_width() as u64)
                    .ok_or("Photoshop row table overflow")?;
                let start = self.position()?;
                let table_end = start
                    .checked_add(bytes)
                    .filter(|n| *n <= end)
                    .ok_or("Truncated Photoshop row table")?;
                let packed = sum_rle_table(self.input, format, rows, row_bytes, &mut [0; 4096], self.check)?;
                if packed != end - table_end {
                    return Err("Photoshop row table does not cover its channel exactly".into());
                }
                counts = Some(Span {
                    offset: start,
                    bytes,
                });
                Compression::Rle
            }
            2 => Compression::Zip,
            3 => Compression::PredictedZip,
            _ => return Err("Unsupported Photoshop compression".into()),
        };
        let payload = Span {
            offset: self.position()?,
            bytes: end - self.position()?,
        };
        let mut row_padding = 0;
        if compression == Compression::Raw {
            let expected = rows
                .checked_mul(row_bytes)
                .ok_or("Photoshop raw size overflow")?;
            if expected != payload.bytes {
                if layer_padding
                    && row_bytes % 2 == 1
                    && rows.checked_mul(row_bytes + 1) == Some(payload.bytes)
                {
                    row_padding = 1;
                } else {
                    return Err("Photoshop raw channel size differs".into());
                }
            }
        }
        if matches!(compression, Compression::Zip | Compression::PredictedZip) && payload.bytes == 0
        {
            return Err("Missing Photoshop ZIP stream".into());
        }
        self.seek(end)?;
        Ok(Encoding {
            span,
            compression,
            rows,
            row_bytes,
            row_padding,
            counts,
            payload,
            depth,
            format,
        })
    }

    fn layers(
        &mut self,
        info: Span,
        header: Header,
        ambiguities: &mut Vec<Ambiguity>,
    ) -> Result<(Vec<Layer>, bool), String> {
        if info.bytes == 0 {
            return Ok((Vec::new(), false));
        }
        let end = info.end()?;
        self.seek(info.offset)?;
        let count = self.u16(end)? as i16;
        let transparency = count < 0;
        let count = usize::from(count.unsigned_abs());
        if count > self.limits.layers {
            return Err("Too many Photoshop layer records".into());
        }
        let mut layers = Vec::with_capacity(count);
        for index in 0..count {
            let record_start = self.position()?;
            let bounds = self.bounds(end)?;
            let dimensions = self.dimensions(bounds)?;
            let channel_count = usize::from(self.u16(end)?);
            if channel_count > self.limits.channels {
                return Err("Too many Photoshop layer channels".into());
            }
            let mut channels = Vec::with_capacity(channel_count);
            let mut ids = HashSet::new();
            for _ in 0..channel_count {
                let id = self.u16(end)? as i16;
                let bytes = self.length(end, header.format.large())?;
                if !ids.insert(id) {
                    ambiguities.push(Ambiguity::LayerChannel(index, id));
                }
                channels.push(Channel {
                    id,
                    span: Span { offset: 0, bytes },
                    dimensions: if (-1..=2).contains(&id) {
                        Some(dimensions)
                    } else {
                        None
                    },
                    encoding: None,
                    encoding_error: None,
                });
            }
            if self.read::<4>(end)? != *b"8BIM" {
                return Err("Invalid Photoshop blend signature".into());
            }
            let blend_mode = self.read(end)?;
            let [opacity, clipping, flags, filler] = self.read(end)?;
            let extra = self.block(end, false)?;
            let next_record = self.position()?;
            self.seek(extra.offset)?;
            let extra_end = extra.end()?;
            let mask = self.block(extra_end, false)?;
            let (mask_dimensions, user_mask_dimensions) = if mask.bytes >= 18 {
                let next = self.position()?;
                self.seek(mask.offset)?;
                let bounds = self.bounds(mask.end()?)?;
                if bounds
                    .iter()
                    .any(|v| v.unsigned_abs() > self.limits.coordinate)
                {
                    return Err("PSD mask coordinates exceed initial limits".into());
                }
                self.seek(mask.offset)?;
                let info = match mask.bytes {
                    20 => super::standard_mask_header(&self.read::<20>(mask.end()?)?),
                    27 => super::standard_mask_header(&self.read::<27>(mask.end()?)?),
                    28 => super::standard_mask_header(&self.read::<28>(mask.end()?)?),
                    36 => super::standard_mask_header(&self.read::<36>(mask.end()?)?),
                    _ => None,
                };
                let result = if let Some(info) = info {
                    let user = if info.rendered_bounds.is_some() {
                        Some(self.mask_dimensions(info.bounds, header.depth)?)
                    } else {
                        None
                    };
                    (Some(self.mask_dimensions(bounds, header.depth)?), user)
                } else {
                    (None, None)
                };
                self.seek(next)?;
                result
            } else {
                (None, None)
            };
            for channel in &mut channels {
                channel.dimensions = match channel.id {
                    -2 => mask_dimensions,
                    -3 => user_mask_dimensions,
                    _ => channel.dimensions,
                };
            }
            let blending_ranges = self.block(extra_end, false)?;
            let length = u64::from(self.read::<1>(extra_end)?[0]);
            let name = self.span(length, extra_end)?;
            self.pad(length + 1, 4, extra_end)?;
            let mut tags = Vec::new();
            let mut keys = HashSet::new();
            while !self.semantic_only && self.position()? < extra_end {
                if extra_end - self.position()? < 12 {
                    // The semantic parser protects nonzero unframed layer
                    // metadata using the saved composite. Preserve those raw
                    // record bytes; indexing must not turn that case editable.
                    self.seek(extra_end)?;
                    break;
                }
                let start = self.position()?;
                let signature = self.read::<4>(extra_end)?;
                self.seek(start)?;
                if !matches!(&signature, b"8BIM" | b"8B64") {
                    self.seek(extra_end)?;
                    break;
                }
                let tag = self.tag(extra_end, header.format, 2)?;
                if !keys.insert(tag.key) {
                    ambiguities.push(Ambiguity::LayerTag(index, tag.key));
                }
                tags.push(tag);
            }
            layers.push(Layer {
                record: Span {
                    offset: record_start,
                    bytes: next_record - record_start,
                },
                bounds,
                blend_mode,
                opacity,
                clipping,
                flags,
                filler,
                extra,
                mask,
                blending_ranges,
                name,
                tags,
                channels,
            });
            self.seek(next_record)?;
        }
        for layer in &mut layers {
            for channel in &mut layer.channels {
                channel.span = self.span(channel.span.bytes, end)?;
                if let Some([width, height]) = channel.dimensions {
                    let result = self.encoding(
                        channel.span,
                        width,
                        height,
                        1,
                        header.depth,
                        header.format,
                        matches!(channel.id, -2 | -3),
                        true,
                    );
                    match result {
                        Ok(encoding) => channel.encoding = Some(encoding),
                        Err(reason) if self.semantic_only => {
                            (self.check)()?;
                            channel.encoding_error = Some(reason);
                            self.seek(channel.span.end()?)?;
                        }
                        Err(reason) => return Err(reason),
                    }
                } else if channel.span.bytes != 0 && !self.semantic_only {
                    let next = self.position()?;
                    self.seek(channel.span.offset)?;
                    if self.u16(channel.span.end()?)? > 3 {
                        return Err("Unsupported Photoshop compression".into());
                    }
                    self.seek(next)?;
                }
            }
        }
        let trailing = end - self.position()?;
        if trailing > 1 {
            // Photoshop also writes layer information rounded to four bytes.
            // Preserve the existing one-byte padding convention separately.
            if trailing > 3 || info.bytes % 4 != 0 {
                return Err("Unframed Photoshop layer information".into());
            }
            self.zeros(end)?;
        }
        self.seek(end)?;
        Ok((layers, transparency))
    }
}

impl<R: Read + Seek, F: FnMut() -> Result<(), String>> Scanner<'_, R, F> {
    fn resource(&mut self, end: u64) -> Result<Resource, String> {
        if self.read::<4>(end)? != *b"8BIM" {
            return Err("Invalid Photoshop resource signature".into());
        }
        let id = self.u16(end)?;
        let length = u64::from(self.read::<1>(end)?[0]);
        let name = self.span(length, end)?;
        self.pad(length + 1, 2, end)?;
        let data = self.block(end, false)?;
        let after = self.position()?;
        let peerbrush_source = if id == 4000 && data.bytes >= 4 {
            self.seek(data.offset)?;
            self.read::<4>(data.end()?)? == *b"PBR1"
        } else {
            false
        };
        self.seek(after)?;
        self.pad(data.bytes, 2, end)?;
        Ok(Resource {
            id,
            name,
            data,
            peerbrush_source,
        })
    }
}

/// Classify only the canonical native writer prefix, including empty chunk
/// lists. Full indexing, stream verification and semantic admission follow on
/// the actual owned decoder handle. This probe allocates no metadata tree.
pub(crate) fn native_manifest_candidate<R: Read + Seek>(
    input: &mut R,
    mut check: impl FnMut() -> Result<(), String>,
) -> Result<bool, String> {
    let file_bytes = input.seek(SeekFrom::End(0)).map_err(|e| e.to_string())?;
    let mut scan = Scanner {
        input,
        check: &mut check,
        limits: Limits::current(),
        tags: 0,
        semantic_only: true,
    };
    if file_bytes < 26 {
        return Ok(false);
    }
    scan.seek(0)?;
    if scan.read::<4>(file_bytes)? != *b"8BPS" {
        return Ok(false);
    }
    scan.seek(26)?;
    scan.block(file_bytes, false)?;
    let section = scan.block(file_bytes, false)?;
    scan.seek(section.offset)?;
    let end = section.end()?;
    let mut native = false;
    let mut saw_effects_visibility = false;
    let mut saw_pixel_aspect = false;
    let mut saw_grid = false;
    let mut saw_layer_id_seed = false;
    let mut saw_print_flags = false;
    let mut saw_print_flags_info = false;
    let mut saw_copyright = false;
    let mut saw_global_angle = false;
    let mut saw_global_altitude = false;
    let mut saw_profile = false;
    let mut untagged = None;
    for _ in 0..scan.limits.resources {
        if scan.position()? == end {
            break;
        }
        let resource = scan.resource(end)?;
        if super::unsupported_document_resource(resource.id).is_some() {
            return Ok(false);
        }
        if resource.id == super::EFFECTS_VISIBILITY_RESOURCE_ID {
            let flag = if resource.data.bytes == 1 {
                let resume = scan.position()?;
                scan.seek(resource.data.offset)?;
                let flag = scan.read::<1>(resource.data.end()?)?[0];
                scan.seek(resume)?;
                Some(flag)
            } else {
                None
            };
            if super::effects_visibility_warning(&mut saw_effects_visibility, flag).is_some() {
                return Ok(false);
            }
        }
        if resource.id == super::PIXEL_ASPECT_RESOURCE_ID {
            if std::mem::replace(&mut saw_pixel_aspect, true) || resource.data.bytes != 12 {
                return Ok(false);
            }
            let resume = scan.position()?;
            scan.seek(resource.data.offset)?;
            let bytes = scan.read::<12>(resource.data.end()?)?;
            scan.seek(resume)?;
            if super::parse_pixel_aspect(&bytes).is_err() {
                return Ok(false);
            }
        }
        if resource.id == super::GRID_RESOURCE_ID {
            if std::mem::replace(&mut saw_grid, true) || resource.data.bytes != 16 {
                return Ok(false);
            }
            let resume = scan.position()?;
            scan.seek(resource.data.offset)?;
            let bytes = scan.read::<16>(resource.data.end()?)?;
            scan.seek(resume)?;
            if super::parse_grid(&bytes).is_err() {
                return Ok(false);
            }
        }
        if resource.id == super::LAYER_ID_SEED_RESOURCE_ID
            && (std::mem::replace(&mut saw_layer_id_seed, true) || resource.data.bytes != 4)
        {
            return Ok(false);
        }
        if resource.id == super::PRINT_FLAGS_RESOURCE_ID {
            if std::mem::replace(&mut saw_print_flags, true) || resource.data.bytes != 9 {
                return Ok(false);
            }
            let resume = scan.position()?;
            scan.seek(resource.data.offset)?;
            let bytes = scan.read::<9>(resource.data.end()?)?;
            scan.seek(resume)?;
            if super::parse_print_flags(&bytes).is_err() {
                return Ok(false);
            }
        }
        if resource.id == super::PRINT_FLAGS_INFO_RESOURCE_ID {
            if std::mem::replace(&mut saw_print_flags_info, true) || resource.data.bytes != 10 {
                return Ok(false);
            }
            let resume = scan.position()?;
            scan.seek(resource.data.offset)?;
            let bytes = scan.read::<10>(resource.data.end()?)?;
            scan.seek(resume)?;
            if super::parse_print_flags_info(&bytes).is_err() {
                return Ok(false);
            }
        }
        if resource.id == super::COPYRIGHT_RESOURCE_ID {
            if std::mem::replace(&mut saw_copyright, true) || resource.data.bytes != 1 {
                return Ok(false);
            }
            let resume = scan.position()?;
            scan.seek(resource.data.offset)?;
            let bytes = scan.read::<1>(resource.data.end()?)?;
            scan.seek(resume)?;
            if super::parse_copyright(&bytes).is_err() {
                return Ok(false);
            }
        }
        if resource.id == super::GLOBAL_ANGLE_RESOURCE_ID {
            if std::mem::replace(&mut saw_global_angle, true) || resource.data.bytes != 4 {
                return Ok(false);
            }
            let resume = scan.position()?;
            scan.seek(resource.data.offset)?;
            let bytes = scan.read::<4>(resource.data.end()?)?;
            scan.seek(resume)?;
            if super::parse_global_angle(&bytes).is_err() {
                return Ok(false);
            }
        }
        if resource.id == super::GLOBAL_ALTITUDE_RESOURCE_ID
            && (std::mem::replace(&mut saw_global_altitude, true) || resource.data.bytes != 4)
        {
            return Ok(false);
        }
        if resource.id == 1039 {
            if std::mem::replace(&mut saw_profile, true) {
                return Ok(false);
            }
            let resume = scan.position()?;
            scan.seek(resource.data.offset)?;
            let working = super::read_working_srgb(
                (&mut *scan.input).take(resource.data.bytes),
                resource.data.bytes,
            )?;
            scan.seek(resume)?;
            if !working {
                return Ok(false);
            }
        }
        if resource.id == super::UNTAGGED_PROFILE_RESOURCE_ID {
            if untagged.is_some() || resource.data.bytes != 1 {
                return Ok(false);
            }
            let resume = scan.position()?;
            scan.seek(resource.data.offset)?;
            let flag = scan.read::<1>(resource.data.end()?)?;
            scan.seek(resume)?;
            let Ok([value]) = super::parse_untagged_profile(&flag) else { return Ok(false) };
            untagged = Some(value);
        }
        if saw_profile && untagged == Some(1) {
            return Ok(false);
        }
        if native || !resource.peerbrush_source {
            continue;
        }
        let resume = scan.position()?;
        scan.seek(resource.data.offset + 4)?;
        let mut input = flate2::read::ZlibDecoder::new(
            (&mut *scan.input).take((resource.data.bytes - 4).min(65536)),
        );
        let mut prefix = [0; 11];
        native = input.read_exact(&mut prefix).is_ok() && prefix == *b"{\"chunks\":[";
        scan.seek(resume)?;
    }
    Ok(native)
}

/// Index the current RGB8/16 stream with bounded metadata and no section-sized buffer.
/// ZIP payload bytes are authenticated/fully consumed by the row reader, not by this index.
pub(crate) fn index<R: Read + Seek>(
    input: &mut R,
    limits: Limits,
    check: impl FnMut() -> Result<(), String>,
) -> Result<Index, String> {
    index_with(input, limits, false, false, check)
}

pub(crate) fn index_protected<R: Read + Seek>(
    input: &mut R,
    check: impl FnMut() -> Result<(), String>,
) -> Result<Index, String> {
    index_with(input, Limits::native_standard(), false, true, check)
}

/// Match the active decoder's semantic resource/record policy without retaining
/// ignored resources or imposing a new cumulative layer-tag admission. Invalid
/// channel encodings stay provisional until the semantic parser admits a layer.
pub(crate) fn index_for_decode<R: Read + Seek>(
    input: &mut R,
    check: impl FnMut() -> Result<(), String>,
) -> Result<Index, String> {
    index_with(input, Limits::current(), true, true, check)
}

fn index_with<R: Read + Seek>(
    input: &mut R,
    limits: Limits,
    semantic_only: bool,
    allow_grayscale: bool,
    mut check: impl FnMut() -> Result<(), String>,
) -> Result<Index, String> {
    check()?;
    let file_bytes = input
        .seek(SeekFrom::End(0))
        .map_err(|_| "Cannot size Photoshop stream")?;
    if file_bytes > limits.file_bytes {
        return Err("Photoshop file exceeds index limits".into());
    }
    let mut scan = Scanner {
        input,
        check: &mut check,
        limits,
        tags: 0,
        semantic_only,
    };
    scan.seek(0)?;
    if scan.read::<4>(file_bytes)? != *b"8BPS" {
        return Err("Invalid Photoshop signature".into());
    }
    let format = match scan.u16(file_bytes)? {
        1 => Format::Psd,
        2 => Format::Psb,
        _ => return Err("Unsupported Photoshop version".into()),
    };
    if scan.read::<6>(file_bytes)? != [0; 6] {
        return Err("Invalid Photoshop header reserved bytes".into());
    }
    let channels = scan.u16(file_bytes)?;
    let height = scan.u32(file_bytes)?;
    let width = scan.u32(file_bytes)?;
    let depth = scan.u16(file_bytes)?;
    let mode = scan.u16(file_bytes)?;
    let color_channels = match (mode, channels) {
        (3, 3..=4) => 3,
        (1, 1..=2) if allow_grayscale => 1,
        _ => return Err("Photoshop header exceeds RGB8/16 index limits".into()),
    };
    if width == 0
        || height == 0
        || width > if format.large() { 300_000 } else { 30_000 }
        || height > if format.large() { 300_000 } else { 30_000 }
        || width > limits.max_edge
        || height > limits.max_edge
        || u64::from(width)
            .checked_mul(u64::from(height))
            .is_none_or(|n| n > limits.pixels)
        || ![8, 16].contains(&depth)
        || u64::from(width) * u64::from(depth / 8) > limits.row_bytes
    {
        return Err("Photoshop header exceeds RGB8/16 index limits".into());
    }
    let header = Header {
        format,
        width,
        height,
        depth,
        channels,
    };
    // Protected grayscale reports need the bounded original record inventory.
    let semantic_only = semantic_only && color_channels == 3;
    scan.semantic_only = semantic_only;
    let color_data = scan.block(file_bytes, false)?;
    if color_channels == 1 && color_data.bytes != 0 {
        return Err("Photoshop grayscale preview has unexpected color-mode data".into());
    }
    let resource_section = scan.block(file_bytes, false)?;
    let next = scan.position()?;
    scan.seek(resource_section.offset)?;
    let mut resources = Vec::new();
    let mut ambiguities = Vec::new();
    let mut peerbrush = false;
    let mut profile = false;
    while scan.position()? < resource_section.end()? {
        if !semantic_only && resources.len() >= limits.resources {
            return Err("Too many Photoshop image resources".into());
        }
        let resource = scan.resource(resource_section.end()?)?;
        let id = resource.id;
        let source = resource.peerbrush_source;
        if source && peerbrush && !ambiguities.contains(&Ambiguity::PeerBrushSources) {
            ambiguities.push(Ambiguity::PeerBrushSources);
        }
        peerbrush |= source;
        if id == 1039 {
            if profile && !ambiguities.contains(&Ambiguity::ColorProfiles) {
                ambiguities.push(Ambiguity::ColorProfiles);
            }
            profile = true;
        }
        if !semantic_only {
            resources.push(resource);
        } else if (source && resources.iter().filter(|old| old.peerbrush_source).count() < 2)
            || (matches!(id, 1039 | super::RESOLUTION_RESOURCE_ID | super::PRINT_SCALE_RESOURCE_ID
                | super::EFFECTS_VISIBILITY_RESOURCE_ID | super::PIXEL_ASPECT_RESOURCE_ID
                | super::VERSION_INFO_RESOURCE_ID | super::UNTAGGED_PROFILE_RESOURCE_ID
                | super::GRID_RESOURCE_ID | super::LAYER_ID_SEED_RESOURCE_ID
                | super::PRINT_FLAGS_RESOURCE_ID | super::PRINT_FLAGS_INFO_RESOURCE_ID
                | super::COPYRIGHT_RESOURCE_ID | super::GLOBAL_ANGLE_RESOURCE_ID
                | super::GLOBAL_ALTITUDE_RESOURCE_ID)
                && resources.iter().filter(|old| old.id == id).count() < 2)
            || (super::file_info::supported(id)
                && resources.iter().filter(|old| old.id == id).count() < 2)
            || super::unsupported_document_resource(id).is_some_and(|reason|
                !resources.iter().any(|old| super::unsupported_document_resource(old.id) == Some(reason)))
        {
            resources.push(resource);
        }
    }
    scan.seek(next)?;
    let layer_and_mask = scan.block(file_bytes, format.large())?;
    let composite_start = scan.position()?;
    let mut primary_layer_info = Span {
        offset: layer_and_mask.offset,
        bytes: 0,
    };
    let mut global_mask = primary_layer_info;
    let mut global_tags = Vec::new();
    if layer_and_mask.bytes != 0 {
        scan.seek(layer_and_mask.offset)?;
        primary_layer_info = scan.block(layer_and_mask.end()?, format.large())?;
        if scan.position()? != layer_and_mask.end()? {
            global_mask = scan.block(layer_and_mask.end()?, false)?;
            let mut keys = HashSet::new();
            while scan.position()? < layer_and_mask.end()? {
                if layer_and_mask.end()? - scan.position()? < 12 {
                    scan.zeros(layer_and_mask.end()?)?;
                    break;
                }
                let tag = scan.tag(layer_and_mask.end()?, format, 4)?;
                if !keys.insert(tag.key) {
                    ambiguities.push(Ambiguity::GlobalTag(tag.key));
                }
                global_tags.push(tag);
            }
        }
    }
    let mut active_layer_info = primary_layer_info;
    for tag in &global_tags {
        if (depth == 8 && tag.key == *b"Layr") || (depth == 16 && tag.key == *b"Lr16") {
            if depth == 8 && primary_layer_info.bytes != 0 {
                ambiguities.push(Ambiguity::PrimaryAndGlobalLayers);
            }
            active_layer_info = tag.data;
        }
    }
    let (layers, mut merged_transparency) =
        scan.layers(active_layer_info, header, &mut ambiguities)?;
    if primary_layer_info.bytes >= 2 {
        scan.seek(primary_layer_info.offset)?;
        merged_transparency |= (scan.u16(primary_layer_info.end()?)? as i16) < 0;
    }
    for tag in &global_tags {
        merged_transparency |=
            (depth == 8 && tag.key == *b"Mtrn") || (depth == 16 && tag.key == *b"Mt16");
        if ((depth == 8 && tag.key == *b"Layr") || (depth == 16 && tag.key == *b"Lr16"))
            && tag.data.bytes >= 2
        {
            scan.seek(tag.data.offset)?;
            merged_transparency |= (scan.u16(tag.data.end()?)? as i16) < 0;
        }
    }
    let composite = scan.encoding(
        Span {
            offset: composite_start,
            bytes: file_bytes - composite_start,
        },
        width,
        height,
        channels,
        depth,
        format,
        false,
        false,
    )?;
    (scan.check)()?;
    if scan
        .input
        .seek(SeekFrom::End(0))
        .map_err(|_| "Cannot size Photoshop stream")?
        != file_bytes
    {
        return Err("Photoshop source length changed during indexing".into());
    }
    Ok(Index {
        file_bytes,
        header,
        color_channels,
        color_data,
        resources,
        layer_and_mask,
        primary_layer_info,
        global_mask,
        global_tags,
        active_layer_info,
        merged_transparency,
        layers,
        composite,
        ambiguities,
    })
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::io::Cursor;

    pub(crate) fn odd_raw_fixture(format: Format, padded_rows: bool) -> Vec<u8> {
        let mut channels = vec![];
        for c in 0..3u8 {
            let mut data = vec![0, 0];
            for y in 0..3u8 {
                for x in 0..3u8 {
                    data.push(c * 70 + x * 3 + y * 11 + 7);
                }
                if padded_rows {
                    data.push(0xda);
                }
            }
            channels.push(data);
        }
        let mut info = 1i16.to_be_bytes().to_vec();
        for v in [0i32, 0, 3, 3] {
            info.extend_from_slice(&v.to_be_bytes());
        }
        info.extend_from_slice(&3u16.to_be_bytes());
        for (c, data) in channels.iter().enumerate() {
            info.extend_from_slice(&(c as i16).to_be_bytes());
            length(&mut info, data.len(), format.large());
        }
        info.extend_from_slice(b"8BIMnorm");
        info.extend_from_slice(&[255, 0, 0, 0]);
        let mut extra = vec![0; 8];
        extra.extend_from_slice(&[3, b'o', b'd', b'd']);
        block(&mut info, &extra, false);
        for data in &channels {
            info.extend_from_slice(data);
        }
        if info.len() % 2 != 0 {
            info.push(0xa5);
        }
        let mut section = vec![];
        block(&mut section, &info, format.large());
        section.extend_from_slice(&0u32.to_be_bytes());
        let mut bytes = b"8BPS".to_vec();
        bytes.extend_from_slice(&(if format.large() { 2u16 } else { 1 }).to_be_bytes());
        bytes.extend_from_slice(&[0; 6]);
        bytes.extend_from_slice(&3u16.to_be_bytes());
        bytes.extend_from_slice(&3u32.to_be_bytes());
        bytes.extend_from_slice(&3u32.to_be_bytes());
        bytes.extend_from_slice(&8u16.to_be_bytes());
        bytes.extend_from_slice(&3u16.to_be_bytes());
        bytes.extend_from_slice(&[0; 8]);
        block(&mut bytes, &section, format.large());
        bytes.extend_from_slice(&[0; 2]);
        for c in 0..3u8 {
            for y in 0..3u8 {
                for x in 0..3u8 {
                    bytes.push(c * 70 + x * 3 + y * 11 + 7);
                }
            }
        }
        bytes
    }

    #[test]
    fn photoshop_four_byte_layer_info_padding_requires_exact_zero_fill() {
        for format in [Format::Psd, Format::Psb] {
            let original = odd_raw_fixture(format, false);
            let indexed = index(&mut Cursor::new(&original), Limits::current(), || Ok(())).unwrap();
            let start = indexed.layer_and_mask.offset as usize - if format.large() { 8 } else { 4 };
            for (tail, accept) in [
                (&[0, 0][..], true),
                (&[0, 1], false),
                (&[0, 0, 0], false),
                (&[0; 6], false),
            ] {
                let mut info = vec![0, 0]; // Empty layer-record list, with a separate merged composite.
                info.extend_from_slice(tail);
                let mut section = Vec::new();
                block(&mut section, &info, format.large());
                section.extend_from_slice(&0u32.to_be_bytes());
                let mut bytes = original[..start].to_vec();
                block(&mut bytes, &section, format.large());
                bytes.extend_from_slice(&original[indexed.composite.span.offset as usize..]);
                assert_eq!(
                    index(&mut Cursor::new(bytes), Limits::current(), || Ok(())).is_ok(),
                    accept
                );
            }
        }
    }

    pub(crate) fn many_resources_fixture() -> Vec<u8> {
        let original = fixture(Format::Psd, 8, false);
        let old = u32::from_be_bytes(original[30..34].try_into().unwrap()) as usize;
        let mut resources = vec![];
        for id in 10_000..15_000u16 {
            resources.extend_from_slice(b"8BIM");
            resources.extend_from_slice(&id.to_be_bytes());
            resources.extend_from_slice(&[0; 6]);
        }
        resources.extend_from_slice(&original[34..34 + old]);
        let mut bytes = original[..30].to_vec();
        block(&mut bytes, &resources, false);
        bytes.extend_from_slice(&original[34 + old..]);
        bytes
    }

    pub(crate) fn many_layer_tags_fixture() -> Vec<u8> {
        let original = fixture(Format::Psd, 8, false);
        let indexed = index(&mut Cursor::new(&original), Limits::current(), || Ok(())).unwrap();
        let extra = indexed.layers[0].extra;
        let info = indexed.active_layer_info;
        let mut tags = vec![];
        for n in 0..5000usize {
            let key = [
                b'X',
                b'A' + (n / 676) as u8,
                b'A' + (n / 26 % 26) as u8,
                b'A' + (n % 26) as u8,
            ];
            tag(&mut tags, Format::Psd, key, &[], 2);
        }
        let mut changed = original[info.offset as usize..info.end().unwrap() as usize].to_vec();
        let insert = (extra.end().unwrap() - info.offset) as usize;
        changed.splice(insert..insert, tags.iter().copied());
        let extra_length_at = (extra.offset - info.offset - 4) as usize;
        changed[extra_length_at..extra_length_at + 4]
            .copy_from_slice(&((extra.bytes as usize + tags.len()) as u32).to_be_bytes());
        let mut section = vec![];
        block(&mut section, &changed, false);
        section.extend_from_slice(
            &original[info.end().unwrap() as usize..indexed.layer_and_mask.end().unwrap() as usize],
        );
        let mut bytes = original[..indexed.layer_and_mask.offset as usize - 4].to_vec();
        block(&mut bytes, &section, false);
        bytes.extend_from_slice(&original[indexed.composite.span.offset as usize..]);
        bytes
    }

    #[test]
    fn decoder_scan_does_not_add_resource_or_cumulative_layer_tag_restrictions() {
        for bytes in [many_resources_fixture(), many_layer_tags_fixture()] {
            assert!(index(&mut Cursor::new(&bytes), Limits::current(), || Ok(())).is_err());
            let indexed = index_for_decode(&mut Cursor::new(&bytes), || Ok(())).unwrap();
            assert_eq!(indexed.layers.len(), 1);
            assert_eq!(indexed.resources.len(), 1);
            assert!(indexed.layers[0].tags.is_empty());
            assert!(indexed.layers[0]
                .channels
                .iter()
                .all(|c| c.encoding.is_some()));
        }
    }

    fn length(out: &mut Vec<u8>, n: usize, large: bool) {
        if large {
            out.extend_from_slice(&(n as u64).to_be_bytes());
        } else {
            out.extend_from_slice(&(n as u32).to_be_bytes());
        }
    }
    fn block(out: &mut Vec<u8>, data: &[u8], large: bool) {
        length(out, data.len(), large);
        out.extend_from_slice(data);
    }
    fn tag(out: &mut Vec<u8>, format: Format, key: [u8; 4], data: &[u8], alignment: usize) {
        out.extend_from_slice(b"8BIM");
        out.extend_from_slice(&key);
        block(out, data, format.tag_large(&key));
        out.resize(
            out.len() + (alignment - data.len() % alignment) % alignment,
            0,
        );
    }
    fn resource(data: &[u8]) -> Vec<u8> {
        let mut out = b"8BIM".to_vec();
        out.extend_from_slice(&4000u16.to_be_bytes());
        out.extend_from_slice(&[0, 0]);
        block(&mut out, data, false);
        if data.len() % 2 != 0 {
            out.push(0);
        }
        out
    }
    fn plane(depth: u16, width: u32, height: u32, channel: u8) -> Vec<u8> {
        let mut out = Vec::new();
        for y in 0..height {
            for x in 0..width {
                let value = if channel == 3 {
                    u16::MAX
                } else {
                    (10001 + x * 173 + y * 211 + u32::from(channel) * 131) as u16
                };
                if depth == 16 {
                    out.extend_from_slice(&value.to_be_bytes());
                } else {
                    out.push((value / 257) as u8);
                }
            }
        }
        out
    }
    pub(crate) fn fixture(format: Format, depth: u16, rle: bool) -> Vec<u8> {
        let (w, h) = (3u32, 257u32);
        let mut encoded = Vec::new();
        for channel in [3, 0, 1, 2] {
            let raw = plane(depth, w, h, channel);
            let mut data = Vec::new();
            data.extend_from_slice(&(u16::from(rle)).to_be_bytes());
            if rle {
                let row = w as usize * usize::from(depth / 8);
                for _ in 0..h {
                    if format.large() {
                        data.extend_from_slice(&((row + 1) as u32).to_be_bytes());
                    } else {
                        data.extend_from_slice(&((row + 1) as u16).to_be_bytes());
                    }
                }
                for row in raw.chunks_exact(row) {
                    data.push((row.len() - 1) as u8);
                    data.extend_from_slice(row);
                }
            } else {
                data.extend_from_slice(&raw);
            }
            encoded.push(data);
        }
        let mut info = Vec::new();
        info.extend_from_slice(&(-1i16).to_be_bytes());
        for v in [0i32, 0, h as i32, w as i32] {
            info.extend_from_slice(&v.to_be_bytes());
        }
        info.extend_from_slice(&4u16.to_be_bytes());
        for (id, channel) in [-1i16, 0, 1, 2].into_iter().zip(&encoded) {
            info.extend_from_slice(&id.to_be_bytes());
            length(&mut info, channel.len(), format.large());
        }
        info.extend_from_slice(b"8BIMnorm");
        info.extend_from_slice(&[255, 0, 0, 0]);
        let mut extra = vec![0; 8];
        extra.extend_from_slice(&[5, b'l', b'a', b'y', b'e', b'r', 0, 0]);
        tag(&mut extra, format, *b"abcd", &[1, 2, 3], 2);
        block(&mut info, &extra, false);
        for data in &encoded {
            info.extend_from_slice(data);
        }
        if info.len() % 2 != 0 {
            info.push(0);
        }
        let mut layers = Vec::new();
        if depth == 16 {
            length(&mut layers, 0, format.large());
            layers.extend_from_slice(&0u32.to_be_bytes());
            tag(&mut layers, format, *b"Lr16", &info, 4);
        } else {
            block(&mut layers, &info, format.large());
            layers.extend_from_slice(&0u32.to_be_bytes());
        }
        tag(&mut layers, format, *b"ZZZZ", &[19, 23], 4);
        let mut out = b"8BPS".to_vec();
        out.extend_from_slice(&(if format.large() { 2u16 } else { 1 }).to_be_bytes());
        out.extend_from_slice(&[0; 6]);
        out.extend_from_slice(&4u16.to_be_bytes());
        out.extend_from_slice(&h.to_be_bytes());
        out.extend_from_slice(&w.to_be_bytes());
        out.extend_from_slice(&depth.to_be_bytes());
        out.extend_from_slice(&3u16.to_be_bytes());
        length(&mut out, 0, false);
        block(&mut out, &resource(b"PBR1bounded fixture"), false);
        block(&mut out, &layers, format.large());
        out.extend_from_slice(&(u16::from(rle)).to_be_bytes());
        if rle {
            let row = w as usize * usize::from(depth / 8);
            for _ in 0..h * 4 {
                if format.large() {
                    out.extend_from_slice(&((row + 1) as u32).to_be_bytes());
                } else {
                    out.extend_from_slice(&((row + 1) as u16).to_be_bytes());
                }
            }
            for channel in 0..4 {
                for row in plane(depth, w, h, channel).chunks_exact(row) {
                    out.push((row.len() - 1) as u8);
                    out.extend_from_slice(row);
                }
            }
        } else {
            for channel in 0..4 {
                out.extend_from_slice(&plane(depth, w, h, channel));
            }
        }
        out
    }

    #[test]
    fn indexes_multitile_rgb8_and_rgb16_psd_and_psb_with_raw_and_rle_channels() {
        for format in [Format::Psd, Format::Psb] {
            for depth in [8, 16] {
                for rle in [false, true] {
                    let bytes = fixture(format, depth, rle);
                    let value =
                        index(&mut Cursor::new(&bytes), Limits::current(), || Ok(())).unwrap();
                    assert_eq!(
                        value.header,
                        Header {
                            format,
                            width: 3,
                            height: 257,
                            depth,
                            channels: 4
                        }
                    );
                    assert!(value.merged_transparency);
                    assert_eq!(value.layers.len(), 1);
                    assert_eq!(value.layers[0].channels.len(), 4);
                    assert_eq!(value.layers[0].blend_mode, *b"norm");
                    assert_eq!(
                        (
                            value.layers[0].opacity,
                            value.layers[0].clipping,
                            value.layers[0].flags,
                            value.layers[0].filler
                        ),
                        (255, 0, 0, 0)
                    );
                    assert_eq!(
                        value.layers[0].record.end().unwrap(),
                        value.layers[0].extra.end().unwrap()
                    );
                    assert_eq!(value.color_data.bytes, 0);
                    assert_eq!(value.global_mask.bytes, 0);
                    assert_eq!(
                        value.layer_and_mask.end().unwrap(),
                        value.composite.span.offset
                    );
                    assert!(value.active_layer_info.bytes > 0);
                    assert_eq!(value.primary_layer_info.bytes == 0, depth == 16);
                    assert_eq!(value.layers[0].tags[0].key, *b"abcd");
                    assert_eq!(value.global_tags.last().unwrap().key, *b"ZZZZ");
                    assert!(value.ambiguities.is_empty());
                    assert!(value.resources[0].peerbrush_source);
                    assert_eq!(
                        value.composite.compression,
                        if rle {
                            Compression::Rle
                        } else {
                            Compression::Raw
                        }
                    );
                    assert_eq!(value.composite.span.end().unwrap(), bytes.len() as u64);
                    for channel in &value.layers[0].channels {
                        assert_eq!(channel.dimensions, Some([3, 257]));
                        assert!(channel.encoding.is_some());
                    }
                }
            }
        }
    }

    #[test]
    fn detects_duplicate_sources_without_choosing_editable_authority() {
        let mut bytes = fixture(Format::Psb, 16, false);
        let resources = u32::from_be_bytes(bytes[30..34].try_into().unwrap()) as usize;
        let extra = resource(b"PBR1another source");
        bytes.splice(34 + resources..34 + resources, extra.clone());
        bytes[30..34].copy_from_slice(&((resources + extra.len()) as u32).to_be_bytes());
        let value = index(&mut Cursor::new(bytes), Limits::current(), || Ok(())).unwrap();
        assert_eq!(value.resources.len(), 2);
        assert!(value.ambiguities.contains(&Ambiguity::PeerBrushSources));
    }

    #[test]
    fn refuses_truncation_overflow_reserved_bytes_and_trailing_composite_data() {
        let bytes = fixture(Format::Psb, 16, true);
        for end in [0, 4, 8, 25, 30, 34, bytes.len() - 1] {
            assert!(index(
                &mut Cursor::new(&bytes[..end]),
                Limits::current(),
                || Ok(())
            )
            .is_err());
        }
        let mut bad = bytes.clone();
        bad[6] = 1;
        assert!(index(&mut Cursor::new(bad), Limits::current(), || Ok(())).is_err());
        let mut bad = bytes.clone();
        let resources = u32::from_be_bytes(bad[30..34].try_into().unwrap()) as usize;
        bad[34 + resources..42 + resources].copy_from_slice(&u64::MAX.to_be_bytes());
        assert!(index(&mut Cursor::new(bad), Limits::current(), || Ok(())).is_err());
        for rle in [false, true] {
            let mut bad = fixture(Format::Psd, 8, rle);
            bad.push(0);
            assert!(index(&mut Cursor::new(bad), Limits::current(), || Ok(())).is_err());
        }
    }

    #[test]
    fn record_budgets_and_cancellation_apply_before_metadata_collection() {
        let bytes = fixture(Format::Psb, 16, false);
        for limits in [
            Limits {
                resources: 0,
                ..Limits::current()
            },
            Limits {
                tags: 0,
                ..Limits::current()
            },
            Limits {
                layers: 0,
                ..Limits::current()
            },
            Limits {
                row_bytes: 5,
                ..Limits::current()
            },
        ] {
            assert!(index(&mut Cursor::new(&bytes), limits, || Ok(())).is_err());
        }
        let mut calls = 0;
        let error = index(&mut Cursor::new(&bytes), Limits::current(), || {
            calls += 1;
            if calls == 20 {
                Err("Synthetic cancellation".into())
            } else {
                Ok(())
            }
        })
        .unwrap_err();
        assert_eq!(error, "Synthetic cancellation");
    }

    #[test]
    fn explicit_index_profiles_cannot_exceed_photoshop_header_dimension_widths() {
        for (format, width) in [(Format::Psd, 30_001u32), (Format::Psb, 300_001)] {
            let mut bytes = fixture(format, 8, false);
            bytes[14..18].copy_from_slice(&1u32.to_be_bytes());
            bytes[18..22].copy_from_slice(&width.to_be_bytes());
            let limits = Limits {
                max_edge: 400_000,
                pixels: 1_000_000,
                row_bytes: 800_000,
                ..Limits::current()
            };
            assert_eq!(
                index(&mut Cursor::new(bytes), limits, || Ok(())).unwrap_err(),
                "Photoshop header exceeds RGB8/16 index limits"
            );
        }
    }

    struct Counted {
        inner: Cursor<Vec<u8>>,
        total: usize,
        max: usize,
        reads: usize,
    }
    impl Read for Counted {
        fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
            self.reads += 1;
            self.max = self.max.max(out.len());
            let n = self.inner.read(out)?;
            self.total += n;
            Ok(n)
        }
    }
    impl Seek for Counted {
        fn seek(&mut self, p: SeekFrom) -> std::io::Result<u64> {
            self.inner.seek(p)
        }
    }

    #[test]
    fn raw_index_never_reads_or_allocates_whole_resource_or_channel_payloads() {
        let mut reader = Counted {
            inner: Cursor::new(fixture(Format::Psb, 16, false)),
            total: 0,
            max: 0,
            reads: 0,
        };
        index(&mut reader, Limits::current(), || Ok(())).unwrap();
        assert!(reader.total < 1024);
        assert!(reader.max <= 8);
    }

    #[test]
    fn packbits_index_reads_the_complete_row_table_in_bounded_batches() {
        let height = 4097usize;
        for format in [Format::Psd, Format::Psb] {
            let width = format.rle_count_width();
            let table_bytes = height * width;
            let mut bytes = 1u16.to_be_bytes().to_vec();
            for _ in 0..height {
                bytes.extend_from_slice(&2u32.to_be_bytes()[4 - width..]);
            }
            bytes.resize(2 + table_bytes + height * 2, 0);
            let span = Span { offset: 0, bytes: bytes.len() as u64 };
            let mut reader = Counted { inner: Cursor::new(bytes), total: 0, max: 0, reads: 0 };
            let encode = |reader: &mut Counted| {
                Scanner { input: reader, check: &mut || Ok(()), limits: Limits::current(),
                    tags: 0, semantic_only: false }
                    .encoding(span, 1, height as u32, 1, 8, format, false, false)
            };
            let encoding = encode(&mut reader).unwrap();
            assert_eq!(encoding.counts, Some(Span { offset: 2, bytes: table_bytes as u64 }));
            assert_eq!(encoding.payload.offset, (2 + table_bytes) as u64);
            assert_eq!(reader.total, 2 + table_bytes);
            assert_eq!(reader.reads, 1 + table_bytes.div_ceil(4096));
            assert_eq!(reader.max, 4096);
            reader.inner.get_mut()[1 + table_bytes] = 3;
            assert!(encode(&mut reader).unwrap_err().contains("does not cover"));
        }
    }

    #[test]
    fn psb_wide_metadata_span_uses_u64_without_reading_its_virtual_payload() {
        // This virtual sparse source proves offset arithmetic only. It is not
        // a dense-image, native filesystem or larger-engine admission receipt.
        struct Sparse {
            length: u64,
            position: u64,
            regions: Vec<(u64, Vec<u8>)>,
            read: usize,
        }
        impl Read for Sparse {
            fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
                let n = out.len().min(
                    usize::try_from(self.length.saturating_sub(self.position))
                        .unwrap_or(usize::MAX),
                );
                out[..n].fill(0);
                for (offset, data) in &self.regions {
                    let start = self.position.max(*offset);
                    let end = (self.position + n as u64).min(*offset + data.len() as u64);
                    if start < end {
                        out[(start - self.position) as usize..(end - self.position) as usize]
                            .copy_from_slice(
                                &data[(start - offset) as usize..(end - offset) as usize],
                            );
                    }
                }
                self.position += n as u64;
                self.read += n;
                Ok(n)
            }
        }
        impl Seek for Sparse {
            fn seek(&mut self, from: SeekFrom) -> std::io::Result<u64> {
                let position = match from {
                    SeekFrom::Start(n) => i128::from(n),
                    SeekFrom::Current(n) => i128::from(self.position) + i128::from(n),
                    SeekFrom::End(n) => i128::from(self.length) + i128::from(n),
                };
                self.position = u64::try_from(position)
                    .map_err(|_| std::io::Error::other("Invalid virtual seek"))?;
                Ok(self.position)
            }
        }
        let payload = u64::from(u32::MAX) + 17;
        let mut prefix = b"8BPS".to_vec();
        prefix.extend_from_slice(&2u16.to_be_bytes());
        prefix.extend_from_slice(&[0; 6]);
        prefix.extend_from_slice(&3u16.to_be_bytes());
        prefix.extend_from_slice(&1u32.to_be_bytes());
        prefix.extend_from_slice(&1u32.to_be_bytes());
        prefix.extend_from_slice(&8u16.to_be_bytes());
        prefix.extend_from_slice(&3u16.to_be_bytes());
        prefix.extend_from_slice(&[0; 8]);
        prefix.extend_from_slice(&(28 + payload).to_be_bytes());
        prefix.extend_from_slice(&[0; 12]);
        prefix.extend_from_slice(b"8B64lnk2");
        prefix.extend_from_slice(&payload.to_be_bytes());
        let composite = prefix.len() as u64 + payload;
        let mut input = Sparse {
            length: composite + 5,
            position: 0,
            regions: vec![(0, prefix.clone()), (composite, vec![0, 0, 17, 23, 29])],
            read: 0,
        };
        assert!(index(&mut input, Limits::current(), || Ok(())).is_err());
        let profile = Limits::native_standard();
        let value = index(&mut input, profile, || Ok(())).unwrap();
        assert_eq!(value.global_tags[0].data.bytes, payload);
        assert_eq!(value.composite.span.offset, composite);
        assert!(input.read < 256);
        prefix[54..58].copy_from_slice(b"8B64");
        prefix[58..62].copy_from_slice(b"ABCD");
        input.regions[0].1 = prefix;
        assert!(
            index(&mut input, profile, || Ok(())).is_err(),
            "An 8B64 signature alone must not widen an unknown key"
        );
    }
}
