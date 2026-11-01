//! Native channel rows over bounded independent cursors into one leased source.
//!
//! Readers can be interleaved by row/stripe, including separate ZIP channel
//! streams, without retaining incomplete RGBA tiles for an entire canvas. These
//! are unpredicted on-disk channel samples; composite white-matte/transparency
//! interpretation remains the existing semantic codec's responsibility.
use super::structural::{Compression, Encoding, Format, Span};
use flate2::{Decompress, FlushDecompress, Status};
use std::cell::RefCell;
use std::io::{self, Read, Seek, SeekFrom};

const TRANSFER: usize = 64 * 1024;
const MAX_ROW_BYTES: u64 = 300_000 * 2; // Native16 at the Photoshop PSB format edge, not app admission.

pub(crate) struct Source<R> {
    input: RefCell<R>,
    bytes: u64,
}

impl<R: Read + Seek> Source<R> {
    pub(crate) fn new(mut input: R, expected_bytes: u64) -> Result<Self, String> {
        if input
            .seek(SeekFrom::End(0))
            .map_err(|_| "Cannot size Photoshop stream")?
            != expected_bytes
        {
            return Err("Photoshop source length differs from its index".into());
        }
        Ok(Self {
            input: RefCell::new(input),
            bytes: expected_bytes,
        })
    }

    /// A separate bounded cursor for existing semantic resource/tag consumers.
    /// It grants no metadata, source-hash or editability authority.
    pub(crate) fn range<'a>(
        &'a self,
        span: Span,
        check: &'a dyn Fn() -> Result<(), String>,
    ) -> Result<Ranged<'a, R>, String> {
        check()?;
        if span.end()? > self.bytes {
            return Err("Photoshop range exceeds indexed source".into());
        }
        Ok(Ranged {
            source: self,
            span,
            position: 0,
            check,
        })
    }

    fn read_at(&self, position: u64, out: &mut [u8]) -> io::Result<usize> {
        if position
            .checked_add(out.len() as u64)
            .is_none_or(|end| end > self.bytes)
        {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "Photoshop read exceeds indexed source",
            ));
        }
        let mut input = self
            .input
            .try_borrow_mut()
            .map_err(|_| io::Error::other("Photoshop stream is already borrowed"))?;
        if input.seek(SeekFrom::Start(position))? != position {
            return Err(io::Error::other("Photoshop stream seek differs"));
        }
        let n = input.read(out)?;
        if n > out.len() {
            return Err(io::Error::other("Invalid Photoshop read result"));
        }
        Ok(n)
    }

    fn exact_at(&self, mut position: u64, mut out: &mut [u8]) -> Result<(), String> {
        while !out.is_empty() {
            let n = self
                .read_at(position, out)
                .map_err(|_| "Cannot read indexed Photoshop stream")?;
            if n == 0 {
                return Err("Truncated indexed Photoshop source".into());
            }
            position += n as u64;
            out = &mut out[n..];
        }
        Ok(())
    }

    pub(crate) fn unchanged_length(&self) -> Result<(), String> {
        let mut input = self
            .input
            .try_borrow_mut()
            .map_err(|_| "Photoshop stream is already borrowed")?;
        if input
            .seek(SeekFrom::End(0))
            .map_err(|_| "Cannot size Photoshop stream")?
            != self.bytes
        {
            return Err("Photoshop source length changed during decoding".into());
        }
        Ok(())
    }

    pub(crate) fn rows<'a>(
        &'a self,
        encoding: &Encoding,
        check: &'a dyn Fn() -> Result<(), String>,
    ) -> Result<Rows<'a, R>, String> {
        check()?;
        if ![8, 16].contains(&encoding.depth)
            || encoding.row_bytes > MAX_ROW_BYTES
            || encoding.row_bytes % u64::from(encoding.depth / 8) != 0
            || encoding.span.end()? > self.bytes
            || encoding.payload.end()? != encoding.span.end()?
            || encoding.payload.offset < encoding.span.offset
        {
            return Err("Invalid indexed Photoshop row geometry".into());
        }
        let range = |span| Ranged {
            source: self,
            span,
            position: 0,
            check,
        };
        let state = if encoding.compression == Compression::Empty {
            if encoding.span.bytes != 0
                || (encoding.rows != 0 && encoding.row_bytes != 0)
                || encoding.counts.is_some()
            {
                return Err("Invalid empty Photoshop channel".into());
            }
            State::Empty
        } else {
            if encoding.span.bytes < 2 {
                return Err("Truncated Photoshop compression header".into());
            }
            let mut kind = [0; 2];
            self.exact_at(encoding.span.offset, &mut kind)?;
            let expected = match encoding.compression {
                Compression::Raw => 0,
                Compression::Rle => 1,
                Compression::Zip => 2,
                Compression::PredictedZip => 3,
                Compression::Empty => unreachable!(),
            };
            if u16::from_be_bytes(kind) != expected {
                return Err("Photoshop compression changed after indexing".into());
            }
            let after = encoding
                .span
                .offset
                .checked_add(2)
                .ok_or("Photoshop channel offset overflow")?;
            match encoding.compression {
                Compression::Raw => {
                    if encoding.counts.is_some()
                        || encoding.payload.offset != after
                        || encoding.rows.checked_mul(encoding.row_bytes)
                            != Some(encoding.payload.bytes)
                    {
                        return Err("Indexed raw Photoshop channel differs".into());
                    }
                    State::Raw(range(encoding.payload))
                }
                Compression::Rle => {
                    let counts = encoding.counts.ok_or("Missing Photoshop row table")?;
                    let count_bytes = if encoding.format == Format::Psb { 4 } else { 2 };
                    if counts.offset != after
                        || encoding.rows.checked_mul(count_bytes) != Some(counts.bytes)
                        || counts.end()? != encoding.payload.offset
                    {
                        return Err("Indexed Photoshop row table differs".into());
                    }
                    State::Rle {
                        input: range(encoding.payload),
                        counts,
                        packed: Vec::new(),
                    }
                }
                Compression::Zip | Compression::PredictedZip => {
                    if encoding.counts.is_some()
                        || encoding.payload.offset != after
                        || encoding.payload.bytes == 0
                    {
                        return Err("Indexed Photoshop ZIP channel differs".into());
                    }
                    State::Zip(Inflater::new(range(encoding.payload))?)
                }
                Compression::Empty => unreachable!(),
            }
        };
        Ok(Rows {
            source: self,
            encoding: encoding.clone(),
            next: 0,
            state,
            check,
            finished: false,
            failed: false,
        })
    }
}

pub(crate) struct Ranged<'a, R> {
    source: &'a Source<R>,
    span: Span,
    position: u64,
    check: &'a dyn Fn() -> Result<(), String>,
}

impl<R: Read + Seek> Seek for Ranged<'_, R> {
    fn seek(&mut self, from: SeekFrom) -> io::Result<u64> {
        (self.check)().map_err(|_| io::Error::other("Photoshop range interrupted"))?;
        let position = match from {
            SeekFrom::Start(n) => i128::from(n),
            SeekFrom::Current(n) => i128::from(self.position) + i128::from(n),
            SeekFrom::End(n) => i128::from(self.span.bytes) + i128::from(n),
        };
        if position < 0 || position > i128::from(self.span.bytes) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Photoshop seek exceeds indexed range",
            ));
        }
        self.position = position as u64;
        Ok(self.position)
    }
}

impl<R: Read + Seek> Read for Ranged<'_, R> {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        (self.check)().map_err(|_| io::Error::other("Photoshop decode interrupted"))?;
        let count = out
            .len()
            .min(TRANSFER)
            .min(usize::try_from(self.span.bytes - self.position).unwrap_or(usize::MAX));
        if count == 0 {
            return Ok(0);
        }
        let position = self
            .span
            .offset
            .checked_add(self.position)
            .ok_or_else(|| io::Error::other("Photoshop range overflow"))?;
        let n = self.source.read_at(position, &mut out[..count])?;
        self.position += n as u64;
        Ok(n)
    }
}

enum State<'a, R> {
    Raw(Ranged<'a, R>),
    Rle {
        input: Ranged<'a, R>,
        counts: Span,
        packed: Vec<u8>,
    },
    Zip(Inflater<'a, R>),
    Empty,
}

struct Inflater<'a, R> {
    input: Ranged<'a, R>,
    decoder: Decompress,
    buffer: Vec<u8>,
    begin: usize,
    end: usize,
    eof: bool,
    ended: bool,
}

impl<'a, R> Inflater<'a, R> {
    fn new(input: Ranged<'a, R>) -> Result<Self, String> {
        let mut buffer = Vec::new();
        buffer
            .try_reserve_exact(TRANSFER)
            .map_err(|_| "Photoshop ZIP transfer allocation failed")?;
        buffer.resize(TRANSFER, 0);
        Ok(Self {
            input,
            decoder: Decompress::new(true),
            buffer,
            begin: 0,
            end: 0,
            eof: false,
            ended: false,
        })
    }
}

impl<R: Read + Seek> Read for Inflater<'_, R> {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        if out.is_empty() || self.ended {
            return Ok(0);
        }
        loop {
            if self.begin == self.end && !self.eof {
                self.end = self.input.read(&mut self.buffer)?;
                self.begin = 0;
                self.eof = self.end == 0;
            }
            let (before_in, before_out) = (self.decoder.total_in(), self.decoder.total_out());
            let status = self
                .decoder
                .decompress(
                    &self.buffer[self.begin..self.end],
                    out,
                    if self.eof {
                        FlushDecompress::Finish
                    } else {
                        FlushDecompress::None
                    },
                )
                .map_err(|_| {
                    io::Error::new(io::ErrorKind::InvalidData, "Invalid Photoshop ZIP stream")
                })?;
            let consumed = (self.decoder.total_in() - before_in) as usize;
            let produced = (self.decoder.total_out() - before_out) as usize;
            self.begin += consumed;
            if status == Status::StreamEnd {
                self.ended = true;
                return Ok(produced);
            }
            if produced != 0 {
                return Ok(produced);
            }
            if consumed == 0 && (self.eof || self.begin != self.end) {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "Incomplete Photoshop ZIP stream",
                ));
            }
        }
    }
}

#[must_use = "All rows and finish must succeed before source publication"]
pub(crate) struct Rows<'a, R> {
    source: &'a Source<R>,
    encoding: Encoding,
    next: u64,
    state: State<'a, R>,
    check: &'a dyn Fn() -> Result<(), String>,
    finished: bool,
    failed: bool,
}

/// Completion of one exact indexed stream, not an editable-source grant.
///
/// Decoded rows and imported blocks are provisional until every required stream
/// has one of these tokens and the caller rechecks its leased source identity and
/// whole-file hash. Tokens cannot outlive their exact source or match a different
/// descriptor. They must never be serialized as recovery/publication authority.
#[must_use = "Check every required stream completion before publishing imported sources"]
pub(crate) struct CompletedRows<'a, R> {
    source: &'a Source<R>,
    encoding: Encoding,
}

impl<R> std::fmt::Debug for CompletedRows<'_, R> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CompletedRows")
            .field("encoded_span", &self.encoding.span)
            .finish_non_exhaustive()
    }
}

impl<R: Read + Seek> CompletedRows<'_, R> {
    pub(crate) fn check(
        &self,
        source: &Source<R>,
        encoding: &Encoding,
        check: &dyn Fn() -> Result<(), String>,
    ) -> Result<(), String> {
        check()?;
        if !std::ptr::eq(self.source, source) || self.encoding != *encoding {
            return Err("Photoshop completion belongs to another source or channel".into());
        }
        source.unchanged_length()?;
        check()
    }
}

impl<'a, R: Read + Seek> Rows<'a, R> {
    /// Read exactly one unpredicted native channel row. Never converts native16 to8bit.
    pub(crate) fn read_row(&mut self, out: &mut [u8]) -> Result<bool, String> {
        if self.failed {
            return Err("Photoshop row stream is failed".into());
        }
        let result = self.read_row_inner(out);
        self.failed |= result.is_err();
        result
    }

    fn read_row_inner(&mut self, out: &mut [u8]) -> Result<bool, String> {
        (self.check)()?;
        if self.finished {
            return Err("Photoshop rows already finished".into());
        }
        if out.len() as u64 != self.encoding.row_bytes {
            return Err("Wrong Photoshop row transfer size".into());
        }
        if self.next == self.encoding.rows {
            return Ok(false);
        }
        let check = self.check;
        let failure = |message: &str| check().err().unwrap_or_else(|| message.into());
        match &mut self.state {
            State::Raw(input) => input
                .read_exact(out)
                .map_err(|_| failure("Truncated raw Photoshop row"))?,
            State::Zip(input) => input
                .read_exact(out)
                .map_err(|_| failure("Invalid or truncated Photoshop ZIP row"))?,
            State::Empty => {}
            State::Rle {
                input,
                counts,
                packed,
            } => {
                let count_bytes = if self.encoding.format == Format::Psb {
                    4u64
                } else {
                    2
                };
                let offset = self
                    .next
                    .checked_mul(count_bytes)
                    .and_then(|n| counts.offset.checked_add(n))
                    .ok_or("Photoshop row table offset overflow")?;
                let mut value = [0; 4];
                self.source
                    .exact_at(offset, &mut value[..count_bytes as usize])?;
                let length = if count_bytes == 4 {
                    u64::from(u32::from_be_bytes(value))
                } else {
                    u64::from(u16::from_be_bytes(value[..2].try_into().unwrap()))
                };
                if length > self.encoding.row_bytes * 2 + 1024 {
                    return Err("Packed Photoshop row exceeds transfer budget".into());
                }
                let length =
                    usize::try_from(length).map_err(|_| "Photoshop row length overflow")?;
                packed.clear();
                packed
                    .try_reserve_exact(length)
                    .map_err(|_| "Photoshop packed row allocation failed")?;
                packed.resize(length, 0);
                input
                    .read_exact(packed)
                    .map_err(|_| failure("Truncated packed Photoshop row"))?;
                let (mut i, mut o) = (0usize, 0usize);
                while i < packed.len() {
                    if i % 4096 == 0 {
                        (self.check)()?;
                    }
                    let packet = packed[i] as i8;
                    i += 1;
                    match packet {
                        0..=127 => {
                            let n = packet as usize + 1;
                            if i + n > packed.len() || o + n > out.len() {
                                return Err("Invalid Photoshop RLE literal".into());
                            }
                            out[o..o + n].copy_from_slice(&packed[i..i + n]);
                            i += n;
                            o += n;
                        }
                        -127..=-1 => {
                            let n = (-i16::from(packet) + 1) as usize;
                            let value = *packed.get(i).ok_or("Truncated Photoshop RLE repeat")?;
                            i += 1;
                            if o + n > out.len() {
                                return Err("Photoshop RLE row overflow".into());
                            }
                            out[o..o + n].fill(value);
                            o += n;
                        }
                        -128 => {}
                    }
                }
                if o != out.len() {
                    return Err("Photoshop RLE row width differs".into());
                }
            }
        }
        if self.encoding.compression == Compression::PredictedZip {
            if self.encoding.depth == 16 {
                let mut previous = 0u16;
                for word in out.chunks_exact_mut(2) {
                    previous = previous.wrapping_add(u16::from_be_bytes([word[0], word[1]]));
                    word.copy_from_slice(&previous.to_be_bytes());
                }
            } else {
                for i in 1..out.len() {
                    out[i] = out[i].wrapping_add(out[i - 1]);
                }
            }
        }
        self.next += 1;
        (self.check)()?;
        Ok(true)
    }

    /// Reject unfinished, oversized, concatenated, truncated or trailing streams.
    /// Successful completion still grants no file/source or editability authority.
    pub(crate) fn finish(&mut self) -> Result<CompletedRows<'a, R>, String> {
        if self.failed {
            return Err("Photoshop row stream is failed".into());
        }
        let result = self.finish_inner();
        self.failed |= result.is_err();
        result
    }

    fn finish_inner(&mut self) -> Result<CompletedRows<'a, R>, String> {
        (self.check)()?;
        if self.finished || self.next != self.encoding.rows {
            return Err("Photoshop row stream was not completed exactly once".into());
        }
        match &mut self.state {
            State::Zip(input) => {
                let mut extra = [0; 1];
                if input.read(&mut extra).map_err(|_| {
                    (self.check)()
                        .err()
                        .unwrap_or_else(|| "Invalid Photoshop ZIP termination".into())
                })? != 0
                    || !input.ended
                    || input.decoder.total_in() != self.encoding.payload.bytes
                    || input.decoder.total_out()
                        != self
                            .encoding
                            .rows
                            .checked_mul(self.encoding.row_bytes)
                            .ok_or("Photoshop decoded size overflow")?
                {
                    return Err("Photoshop ZIP stream did not end exactly".into());
                }
            }
            State::Raw(input) | State::Rle { input, .. } => {
                if input.position != input.span.bytes {
                    return Err("Photoshop encoded rows contain trailing data".into());
                }
            }
            State::Empty => {}
        }
        self.source.unchanged_length()?;
        (self.check)()?;
        self.finished = true;
        Ok(CompletedRows {
            source: self.source,
            encoding: self.encoding.clone(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use flate2::{write::ZlibEncoder, Compression as Level};
    use std::cell::Cell;
    use std::io::{Cursor, Write};

    fn raw(depth: u16, width: u32, height: u32, channel: u16) -> Vec<u8> {
        let mut bytes = Vec::new();
        for y in 0..height {
            for x in 0..width {
                let word = 10001u32
                    .wrapping_add(x * 173)
                    .wrapping_add(y * 211)
                    .wrapping_add(u32::from(channel) * 131) as u16;
                if depth == 16 {
                    bytes.extend_from_slice(&word.to_be_bytes());
                } else {
                    bytes.push((word / 257) as u8);
                }
            }
        }
        bytes
    }

    fn zip(bytes: &[u8]) -> Vec<u8> {
        let mut out = ZlibEncoder::new(Vec::new(), Level::fast());
        out.write_all(bytes).unwrap();
        out.finish().unwrap()
    }

    fn plane(
        format: Format,
        depth: u16,
        width: u32,
        height: u32,
        kind: u16,
        channel: u16,
    ) -> (Vec<u8>, Encoding, Vec<u8>) {
        let original = raw(depth, width, height, channel);
        let row_bytes = width as usize * usize::from(depth / 8);
        let mut bytes = kind.to_be_bytes().to_vec();
        let mut counts = None;
        if kind == 0 {
            bytes.extend_from_slice(&original);
        } else if kind == 1 {
            let mut packed = Vec::new();
            for row in original.chunks_exact(row_bytes) {
                let start = packed.len();
                for packet in row.chunks(128) {
                    packed.push((packet.len() - 1) as u8);
                    packed.extend_from_slice(packet);
                }
                let length = packed.len() - start;
                if format == Format::Psb {
                    bytes.extend_from_slice(&(length as u32).to_be_bytes());
                } else {
                    bytes.extend_from_slice(&(length as u16).to_be_bytes());
                }
            }
            counts = Some(Span {
                offset: 2,
                bytes: bytes.len() as u64 - 2,
            });
            bytes.extend_from_slice(&packed);
        } else {
            let mut data = original.clone();
            if kind == 3 {
                for row in data.chunks_exact_mut(row_bytes) {
                    if depth == 16 {
                        let mut previous = 0u16;
                        for word in row.chunks_exact_mut(2) {
                            let current = u16::from_be_bytes([word[0], word[1]]);
                            word.copy_from_slice(&current.wrapping_sub(previous).to_be_bytes());
                            previous = current;
                        }
                    } else {
                        for x in (1..row.len()).rev() {
                            row[x] = row[x].wrapping_sub(row[x - 1]);
                        }
                    }
                }
            }
            bytes.extend_from_slice(&zip(&data));
        }
        let offset = counts.map_or(2, |span| span.end().unwrap());
        let encoding = Encoding {
            span: Span {
                offset: 0,
                bytes: bytes.len() as u64,
            },
            compression: match kind {
                0 => Compression::Raw,
                1 => Compression::Rle,
                2 => Compression::Zip,
                _ => Compression::PredictedZip,
            },
            rows: u64::from(height),
            row_bytes: row_bytes as u64,
            counts,
            payload: Span {
                offset,
                bytes: bytes.len() as u64 - offset,
            },
            depth,
            format,
        };
        (bytes, encoding, original)
    }

    fn relocate(encoding: &mut Encoding, offset: u64) {
        encoding.span.offset += offset;
        encoding.payload.offset += offset;
        if let Some(counts) = &mut encoding.counts {
            counts.offset += offset;
        }
    }

    #[test]
    fn streams_native8_and_native16_raw_rle_zip_and_prediction_exactly() {
        for format in [Format::Psd, Format::Psb] {
            for depth in [8, 16] {
                for kind in 0..=3 {
                    let (bytes, encoding, expected) = plane(format, depth, 259, 513, kind, 2);
                    let source = Source::new(Cursor::new(&bytes), bytes.len() as u64).unwrap();
                    let check = || Ok(());
                    let mut input = source.rows(&encoding, &check).unwrap();
                    let mut out = vec![0; encoding.row_bytes as usize];
                    for row in expected.chunks_exact(out.len()) {
                        assert!(input.read_row(&mut out).unwrap());
                        assert_eq!(out, row);
                    }
                    assert!(!input.read_row(&mut out).unwrap());
                    input
                        .finish()
                        .unwrap()
                        .check(&source, &encoding, &check)
                        .unwrap();
                    assert!(input.finish().is_err());
                    assert!(input.read_row(&mut out).is_err());
                }
            }
        }
    }

    #[test]
    fn interleaves_independent_zip_predicted_rle_and_raw_cursors_on_one_source() {
        let mut bytes = Vec::new();
        let mut plans = Vec::new();
        let mut expected = Vec::new();
        for (channel, kind) in [2, 3, 1, 0].into_iter().enumerate() {
            let (data, mut plan, raw) = plane(Format::Psb, 16, 259, 513, kind, channel as u16);
            relocate(&mut plan, bytes.len() as u64);
            bytes.extend_from_slice(&data);
            plans.push(plan);
            expected.push(raw);
        }
        let source = Source::new(Cursor::new(&bytes), bytes.len() as u64).unwrap();
        let check = || Ok(());
        let mut inputs = plans
            .iter()
            .map(|plan| source.rows(plan, &check).unwrap())
            .collect::<Vec<_>>();
        let mut rows = vec![vec![0u8; 518]; 4];
        for y in 0..513 {
            for channel in 0..4 {
                assert!(inputs[channel].read_row(&mut rows[channel]).unwrap());
                assert_eq!(rows[channel], expected[channel][y * 518..(y + 1) * 518]);
            }
        }
        for (input, plan) in inputs.iter_mut().zip(&plans) {
            input
                .finish()
                .unwrap()
                .check(&source, plan, &check)
                .unwrap();
        }
    }

    #[test]
    fn real_index_and_row_readers_agree_for_multitile_native_psd_and_psb() {
        for format in [Format::Psd, Format::Psb] {
            for depth in [8, 16] {
                for rle in [false, true] {
                    let bytes = super::super::structural::tests::fixture(format, depth, rle);
                    let index = super::super::structural::index(
                        &mut Cursor::new(&bytes),
                        super::super::structural::Limits::current(),
                        || Ok(()),
                    )
                    .unwrap();
                    let source = Source::new(Cursor::new(&bytes), index.file_bytes).unwrap();
                    let check = || Ok(());
                    for channel in &index.layers[0].channels {
                        let encoding = channel.encoding.as_ref().unwrap();
                        let mut input = source.rows(encoding, &check).unwrap();
                        let mut row = vec![0; encoding.row_bytes as usize];
                        for y in 0..257 {
                            assert!(input.read_row(&mut row).unwrap());
                            for x in 0..3 {
                                let value = if channel.id == -1 {
                                    u16::MAX
                                } else {
                                    (10001 + x * 173 + y * 211 + channel.id as u32 * 131) as u16
                                };
                                if depth == 16 {
                                    assert_eq!(
                                        &row[x as usize * 2..x as usize * 2 + 2],
                                        &value.to_be_bytes()
                                    );
                                } else {
                                    assert_eq!(row[x as usize], (value / 257) as u8);
                                }
                            }
                        }
                        input
                            .finish()
                            .unwrap()
                            .check(&source, encoding, &check)
                            .unwrap();
                    }
                }
            }
        }
    }

    #[test]
    fn zip_requires_verified_stream_end_checksum_exact_output_and_no_trailing_stream() {
        let (valid, encoding, raw) = plane(Format::Psb, 16, 259, 513, 2, 1);
        let mut variants = Vec::new();
        for missing in [1, 2, 4, 8] {
            variants.push(valid[..valid.len() - missing].to_vec());
        }
        let mut bad = valid.clone();
        *bad.last_mut().unwrap() ^= 1;
        variants.push(bad);
        let mut trailing = valid.clone();
        trailing.extend_from_slice(&[0, 19]);
        variants.push(trailing);
        let mut concatenated = valid.clone();
        concatenated.extend_from_slice(&zip(&[]));
        variants.push(concatenated);
        let mut extra = raw.clone();
        extra.push(7);
        let mut bad = 2u16.to_be_bytes().to_vec();
        bad.extend_from_slice(&zip(&extra));
        variants.push(bad);
        for bytes in variants {
            let mut plan = encoding.clone();
            plan.span.bytes = bytes.len() as u64;
            plan.payload.bytes = bytes.len() as u64 - 2;
            let source = Source::new(Cursor::new(&bytes), bytes.len() as u64).unwrap();
            let check = || Ok(());
            let mut input = source.rows(&plan, &check).unwrap();
            let mut out = vec![0; plan.row_bytes as usize];
            let mut failed = false;
            for _ in 0..plan.rows {
                if input.read_row(&mut out).is_err() {
                    failed = true;
                    break;
                }
            }
            if !failed {
                assert!(input.finish().is_err());
            }
            assert!(input.finish().is_err());
            assert!(input.read_row(&mut out).is_err());
        }
    }

    #[test]
    fn malformed_rle_and_unfinished_or_changed_ranges_cannot_complete() {
        let (mut bytes, plan, _) = plane(Format::Psd, 8, 3, 2, 1, 0);
        bytes[plan.payload.offset as usize] = 127;
        let source = Source::new(Cursor::new(&bytes), bytes.len() as u64).unwrap();
        let check = || Ok(());
        let mut input = source.rows(&plan, &check).unwrap();
        assert!(input.read_row(&mut [0; 3]).is_err());
        assert!(input.finish().is_err());
        let (bytes, plan, _) = plane(Format::Psb, 16, 3, 2, 0, 0);
        let source = Source::new(Cursor::new(bytes.clone()), bytes.len() as u64).unwrap();
        let mut input = source.rows(&plan, &check).unwrap();
        assert!(input.finish().is_err());
        assert!(input.read_row(&mut [0; 6]).is_err());
        let mut input = source.rows(&plan, &check).unwrap();
        assert!(input.read_row(&mut [0; 5]).is_err());
        assert!(input.read_row(&mut [0; 6]).is_err());
        let mut changed = plan.clone();
        changed.payload.offset += 1;
        assert!(source.rows(&changed, &check).is_err());
        let mut changed = plan.clone();
        changed.compression = Compression::Zip;
        assert!(source.rows(&changed, &check).is_err());
        let mut input = source.rows(&plan, &check).unwrap();
        for _ in 0..2 {
            input.read_row(&mut [0; 6]).unwrap();
        }
        source.input.borrow_mut().get_mut().push(0);
        assert!(input.finish().is_err());
    }

    #[test]
    fn cancellation_is_sticky_for_the_reader_and_does_not_mutate_the_source() {
        let (bytes, plan, _) = plane(Format::Psb, 16, 259, 513, 2, 0);
        let source = Source::new(Cursor::new(bytes.clone()), bytes.len() as u64).unwrap();
        let cancelled = Cell::new(false);
        let check = || {
            if cancelled.get() {
                Err("Synthetic cancellation".into())
            } else {
                Ok(())
            }
        };
        let mut input = source.rows(&plan, &check).unwrap();
        let mut row = vec![0; plan.row_bytes as usize];
        input.read_row(&mut row).unwrap();
        cancelled.set(true);
        assert_eq!(
            input.read_row(&mut row).unwrap_err(),
            "Synthetic cancellation"
        );
        cancelled.set(false);
        assert!(input.read_row(&mut row).is_err());
        assert!(input.finish().is_err());
        assert_eq!(source.input.borrow().get_ref(), &bytes);
    }

    #[test]
    fn zip_source_reads_are_bounded_and_rows_do_not_materialize_whole_planes() {
        struct Counted<'a> {
            input: Cursor<Vec<u8>>,
            max: &'a Cell<usize>,
        }
        impl Read for Counted<'_> {
            fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
                self.max.set(self.max.get().max(out.len()));
                self.input.read(out)
            }
        }
        impl Seek for Counted<'_> {
            fn seek(&mut self, from: SeekFrom) -> io::Result<u64> {
                self.input.seek(from)
            }
        }
        let (bytes, plan, _) = plane(Format::Psb, 16, 259, 513, 2, 0);
        let max = Cell::new(0);
        let source = Source::new(
            Counted {
                input: Cursor::new(bytes.clone()),
                max: &max,
            },
            bytes.len() as u64,
        )
        .unwrap();
        let check = || Ok(());
        let mut input = source.rows(&plan, &check).unwrap();
        let mut row = vec![0; plan.row_bytes as usize];
        for _ in 0..plan.rows {
            input.read_row(&mut row).unwrap();
        }
        input
            .finish()
            .unwrap()
            .check(&source, &plan, &check)
            .unwrap();
        assert!(max.get() <= TRANSFER);
        if let State::Zip(zip) = &input.state {
            assert_eq!(zip.buffer.len(), TRANSFER);
        } else {
            panic!("Expected bounded ZIP state");
        }
    }

    #[test]
    fn completion_tokens_bind_the_exact_source_plan_and_later_checks() {
        let (bytes, plan, _) = plane(Format::Psb, 16, 3, 2, 2, 0);
        let source = Source::new(Cursor::new(bytes.clone()), bytes.len() as u64).unwrap();
        let second = Source::new(Cursor::new(bytes.clone()), bytes.len() as u64).unwrap();
        let cancelled = Cell::new(false);
        let check = || {
            if cancelled.get() {
                Err("Synthetic cancellation".into())
            } else {
                Ok(())
            }
        };
        let mut input = source.rows(&plan, &check).unwrap();
        for _ in 0..2 {
            input.read_row(&mut [0; 6]).unwrap();
        }
        let completed = input.finish().unwrap();
        completed.check(&source, &plan, &check).unwrap();
        assert!(completed.check(&second, &plan, &check).is_err());
        let mut changed = plan.clone();
        changed.depth = 8;
        assert!(completed.check(&source, &changed, &check).is_err());
        cancelled.set(true);
        assert_eq!(
            completed.check(&source, &plan, &check).unwrap_err(),
            "Synthetic cancellation"
        );
        cancelled.set(false);
        source.input.borrow_mut().get_mut().push(0);
        assert!(completed.check(&source, &plan, &check).is_err());
    }

    #[test]
    fn bounded_metadata_ranges_cannot_seek_into_adjacent_data_or_disturb_channel_cursors() {
        let (mut bytes, mut plan, expected) = plane(Format::Psb, 16, 3, 2, 2, 0);
        let prefix = vec![17, 23, 29, 31];
        relocate(&mut plan, prefix.len() as u64);
        bytes.splice(0..0, prefix.clone());
        let source = Source::new(Cursor::new(&bytes), bytes.len() as u64).unwrap();
        let check = || Ok(());
        let mut metadata = source
            .range(
                Span {
                    offset: 0,
                    bytes: 4,
                },
                &check,
            )
            .unwrap();
        let mut rows = source.rows(&plan, &check).unwrap();
        let mut row = [0; 6];
        rows.read_row(&mut row).unwrap();
        assert_eq!(&row, &expected[..6]);
        let mut value = [0; 2];
        metadata.read_exact(&mut value).unwrap();
        assert_eq!(value, [17, 23]);
        metadata.seek(SeekFrom::End(-1)).unwrap();
        metadata.read_exact(&mut value[..1]).unwrap();
        assert_eq!(value[0], 31);
        assert_eq!(metadata.read(&mut value).unwrap(), 0);
        assert!(metadata.seek(SeekFrom::Current(1)).is_err());
        assert!(metadata.seek(SeekFrom::Start(5)).is_err());
        rows.read_row(&mut row).unwrap();
        assert_eq!(&row, &expected[6..]);
        rows.finish()
            .unwrap()
            .check(&source, &plan, &check)
            .unwrap();
        assert!(source
            .range(
                Span {
                    offset: bytes.len() as u64,
                    bytes: 1
                },
                &check
            )
            .is_err());
    }

    #[test]
    fn psb_rle_uses_full_width_row_lengths_above_u16_without_raising_app_limits() {
        let (bytes, plan, expected) = plane(Format::Psb, 16, 40_000, 1, 1, 0);
        assert!(u32::from_be_bytes(bytes[2..6].try_into().unwrap()) > u32::from(u16::MAX));
        let source = Source::new(Cursor::new(&bytes), bytes.len() as u64).unwrap();
        let check = || Ok(());
        let mut rows = source.rows(&plan, &check).unwrap();
        let mut row = vec![0; 80_000];
        rows.read_row(&mut row).unwrap();
        assert_eq!(row, expected);
        rows.finish()
            .unwrap()
            .check(&source, &plan, &check)
            .unwrap();
        let mut wrong = plan.clone();
        wrong.format = Format::Psd;
        assert!(source.rows(&wrong, &check).is_err());
    }

    #[test]
    fn indexed_zip_composites_finish_one_planar_stream_across_all_channels() {
        for format in [Format::Psd, Format::Psb] {
            for depth in [8, 16] {
                for kind in [2u16, 3] {
                    let (width, height) = (259u32, 257u32);
                    let row_bytes = width as usize * usize::from(depth / 8);
                    let expected = (0..4)
                        .flat_map(|channel| raw(depth, width, height, channel))
                        .collect::<Vec<_>>();
                    let mut predicted = expected.clone();
                    if kind == 3 {
                        for row in predicted.chunks_exact_mut(row_bytes) {
                            if depth == 16 {
                                let mut previous = 0u16;
                                for sample in row.chunks_exact_mut(2) {
                                    let word = u16::from_be_bytes([sample[0], sample[1]]);
                                    sample.copy_from_slice(
                                        &word.wrapping_sub(previous).to_be_bytes(),
                                    );
                                    previous = word;
                                }
                            } else {
                                for x in (1..row.len()).rev() {
                                    row[x] = row[x].wrapping_sub(row[x - 1]);
                                }
                            }
                        }
                    }
                    let mut bytes = b"8BPS".to_vec();
                    bytes.extend_from_slice(
                        &(if format == Format::Psb { 2u16 } else { 1 }).to_be_bytes(),
                    );
                    bytes.extend_from_slice(&[0; 6]);
                    bytes.extend_from_slice(&4u16.to_be_bytes());
                    bytes.extend_from_slice(&height.to_be_bytes());
                    bytes.extend_from_slice(&width.to_be_bytes());
                    bytes.extend_from_slice(&depth.to_be_bytes());
                    bytes.extend_from_slice(&3u16.to_be_bytes());
                    bytes.extend_from_slice(&[0; 8]);
                    if format == Format::Psb {
                        bytes.extend_from_slice(&[0; 8]);
                    } else {
                        bytes.extend_from_slice(&[0; 4]);
                    }
                    bytes.extend_from_slice(&kind.to_be_bytes());
                    bytes.extend_from_slice(&zip(&predicted));
                    let index = super::super::structural::index(
                        &mut Cursor::new(&bytes),
                        super::super::structural::Limits::current(),
                        || Ok(()),
                    )
                    .unwrap();
                    assert_eq!(index.composite.rows, u64::from(height) * 4);
                    let source = Source::new(Cursor::new(&bytes), index.file_bytes).unwrap();
                    let check = || Ok(());
                    let mut rows = source.rows(&index.composite, &check).unwrap();
                    let mut row = vec![0; row_bytes];
                    for expected in expected.chunks_exact(row_bytes) {
                        assert!(rows.read_row(&mut row).unwrap());
                        assert_eq!(row, expected);
                    }
                    rows.finish()
                        .unwrap()
                        .check(&source, &index.composite, &check)
                        .unwrap();
                }
            }
        }
    }
}
