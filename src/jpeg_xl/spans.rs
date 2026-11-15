//! Native JPEG XL partial scanlines. Only the parent owns disk staging.
use super::*;
use crate::{
    engine::native::{NativeDocument, StagedOutput},
    large_document::{ByteRole, Reservation, ScratchBytes},
    selection::NativeRows,
    workspace::source_runtime::{SessionTemporary, SourceSession},
};
use image::metadata::Orientation;

const CHUNK: usize = stream_chunks::CHUNK;
const ROW_BYTES: usize = crate::raster::NATIVE_EDGE as usize * 8;

pub(super) fn orient(info: Info, orientation: u8) -> Result<Info, String> {
    match orientation {
        1..=4 => Ok(info),
        5..=8 => Ok(Info {
            width: info.height,
            height: info.width,
            ..info
        }),
        _ => Err(PROTOCOL.into()),
    }
}

fn span_bytes(info: Info, x: u32, y: u32, count: u32) -> Result<usize, String> {
    if y >= info.height || count == 0 || x.checked_add(count).is_none_or(|end| end > info.width) {
        return Err(PROTOCOL.into());
    }
    Ok(count as usize * 4 * usize::from(info.bit_depth / 8))
}

pub(super) struct Writer<'a> {
    request: &'a Request,
    output: &'a mut dyn Write,
    raw: Option<Info>,
    sent: usize,
    failure: Option<String>,
}

impl Writer<'_> {
    pub(super) fn start(
        &mut self,
        raw: Info,
        orientation: u8,
        profile: &[u8],
    ) -> Result<(), String> {
        if self.raw.is_some() {
            return Err(PROTOCOL.into());
        }
        let mut response = self.request.response(Kind::Result);
        response.info = Some(orient(raw, orientation)?);
        response.profile_bytes = profile.len() as u32;
        response.payload_bytes = self.request.operation.raw_size(raw)? as u64;
        response.validate(self.request)?;
        self.raw = Some(raw);
        header(&mut self.output, &response)?;
        write(&mut self.output, profile)?;
        write(&mut self.output, &[orientation])
    }

    pub(super) fn check(&mut self) -> Result<(), String> {
        self.failure.take().map_or(Ok(()), Err)
    }

    fn span(
        &mut self,
        x: usize,
        y: usize,
        count: usize,
        pixels: *const std::ffi::c_void,
    ) -> Result<(), String> {
        let raw = self.raw.ok_or(PROTOCOL)?;
        let x = u32::try_from(x).map_err(|_| PROTOCOL)?;
        let y = u32::try_from(y).map_err(|_| PROTOCOL)?;
        let count = u32::try_from(count).map_err(|_| PROTOCOL)?;
        let length = span_bytes(raw, x, y, count)?;
        if pixels.is_null() || self.sent + length > self.request.operation.raw_size(raw)? {
            return Err(PROTOCOL.into());
        }
        let mut coordinates = [0; 12];
        for (field, value) in coordinates.chunks_exact_mut(4).zip([x, y, count]) {
            field.copy_from_slice(&value.to_le_bytes());
        }
        write(&mut self.output, &coordinates)?;
        // libjxl lends exactly this span until the synchronous callback returns.
        write(&mut self.output, unsafe {
            std::slice::from_raw_parts(pixels.cast(), length)
        })?;
        self.sent += length;
        Ok(())
    }
}

pub(super) extern "C" fn callback(
    context: *mut std::ffi::c_void,
    x: usize,
    y: usize,
    count: usize,
    pixels: *const std::ffi::c_void,
) {
    // The decoder has no parallel runner. This state outlives its destruction.
    let writer = unsafe { &mut *context.cast::<Writer<'_>>() };
    if writer.failure.is_none() {
        writer.failure = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            writer.span(x, y, count, pixels)
        }))
        .unwrap_or_else(|_| Err(FAILED.into()))
        .err();
    }
}

pub(super) fn write_decoded(
    request: &Request,
    encoded: &mut native::EncodedInput<'_, '_>,
    output: &mut impl Write,
) -> Result<(), String> {
    let mut output = std::io::BufWriter::with_capacity(CHUNK, output);
    let mut writer = Writer {
        request,
        output: &mut output,
        raw: None,
        sent: 0,
        failure: None,
    };
    let decoded = native::decode_spans(encoded, request.info.ok_or(PROTOCOL)?, &mut writer);
    encoded.finish()?;
    match decoded {
        Ok(()) => {
            let raw = writer.raw.ok_or(PROTOCOL)?;
            if writer.sent != request.operation.raw_size(raw)? {
                return Err(PROTOCOL.into());
            }
            writer.output.flush().map_err(|_| FAILED.into())
        }
        Err(reason) if writer.raw.is_none() => {
            let mut response = request.response(Kind::Result);
            response.error = Some(
                if reason.to_ascii_lowercase().contains("profile") || reason.contains("ICC") {
                    ErrorCode::UnsupportedProfile
                } else {
                    ErrorCode::InvalidOrUnsupported
                },
            );
            header(&mut writer.output, &response)?;
            writer.output.flush().map_err(|_| FAILED.into())
        }
        Err(reason) => Err(reason),
    }
}

struct Span {
    x: u16,
    y: u16,
    count: u16,
    offset: u32,
}

pub(super) struct Received<'session> {
    spool: SessionTemporary<'session>,
    index: Vec<Span>,
    length: usize,
    raw: Info,
    orientation: Orientation,
    _index_charge: Reservation,
}

pub(super) fn receive<'session>(
    base: &NativeDocument,
    session: &'session SourceSession,
    input: &mut impl Read,
    display: Info,
    check: &dyn Fn() -> Result<(), String>,
) -> Result<Received<'session>, String> {
    let mut code = [0];
    read(input, &mut code)?;
    let raw = orient(display, code[0])?;
    let orientation = Orientation::from_exif(code[0]).ok_or(PROTOCOL)?;
    let length = image_bytes(raw, CanvasLimits::Native)?;
    let pixel_bytes = 4 * usize::from(raw.bit_depth / 8);
    let pool = base.budget();
    // The pinned render pipeline emits horizontal group fragments. Bound
    // metadata independently of payload size, even for a compromised worker.
    let max_spans = ((raw.width as usize).div_ceil(256) * 4 + 4)
        .saturating_mul(raw.height as usize)
        .min(length / pixel_bytes);
    let index_charge = pool
        .reserve_bytes(max_spans * std::mem::size_of::<Span>(), 64 * 1024 * 1024)
        .map_err(|e| e.to_string())?;
    let mut index = Vec::new();
    index.try_reserve_exact(max_spans).map_err(|_| PROTOCOL)?;
    if index.capacity() > max_spans {
        return Err(PROTOCOL.into());
    }
    let mut span = ScratchBytes::new(pool, ROW_BYTES, ROW_BYTES).map_err(|e| e.to_string())?;
    let mut pending = ScratchBytes::new(pool, CHUNK, CHUNK).map_err(|e| e.to_string())?;
    let mut spool = session.byte_temporary(length as u64, ByteRole::SourceBulk)?;
    let received = (|| {
        check()?;
        let mut total = 0;
        let mut buffered = 0;
        while total < length {
            // Pipe reads observe worker cancellation. Authenticate the owned
            // source when each complete transfer buffer is appended.
            let mut coordinates = [0u8; 12];
            read(input, &mut coordinates)?;
            let [x, y, count] =
                [0, 4, 8].map(|i| u32::from_le_bytes(coordinates[i..i + 4].try_into().unwrap()));
            let size = span_bytes(raw, x, y, count)?;
            if index.len() == max_spans || size > length - total {
                return Err(PROTOCOL.into());
            }
            index.push(Span {
                x: x as u16,
                y: y as u16,
                count: count as u16,
                offset: total as u32,
            });
            read(input, &mut span[..size])?;
            let mut bytes = &span[..size];
            while !bytes.is_empty() {
                let take = bytes.len().min(CHUNK - buffered);
                pending[buffered..buffered + take].copy_from_slice(&bytes[..take]);
                buffered += take;
                bytes = &bytes[take..];
                if buffered == CHUNK {
                    spool.append(&pending, &mut || check())?;
                    buffered = 0;
                }
            }
            total += size;
        }
        if buffered != 0 {
            spool.append(&pending[..buffered], &mut || check())?;
        }
        eof(input)?;
        check()
    })();
    if let Err(reason) = received {
        return match spool.abort(&mut || session.cleanup_check()) {
            Ok(()) => Err(reason),
            Err(cleanup) => Err(format!(
                "{reason}; JPEG XL span spool retirement refused: {cleanup}"
            )),
        };
    }
    Ok(Received {
        spool,
        index,
        length,
        raw,
        orientation,
        _index_charge: index_charge,
    })
}

impl Received<'_> {
    pub(super) fn abort(self, session: &SourceSession) -> Result<(), String> {
        self.spool.abort(&mut || session.cleanup_check())
    }

    /// Replay after the worker's exact EOF and successful exit. The same
    /// source and cancellation checks guard sealing, replay, and retirement.
    pub(super) fn stage(
        self,
        base: &NativeDocument,
        session: &SourceSession,
        source_sha256: [u8; 32],
        check: &dyn Fn() -> Result<(), String>,
    ) -> Result<(StagedOutput, Info, Orientation), String> {
        let Self {
            spool,
            mut index,
            length,
            raw,
            orientation,
            _index_charge,
        } = self;
        let pixel_bytes = 4 * usize::from(raw.bit_depth / 8);
        let pool = base.budget();
        let spool = spool.finish(source_sha256, &mut || check())?;
        index.sort_unstable_by_key(|span| (span.y, span.x));
        let output = crate::retained::transaction::stage(base, session, check, |store, guard| {
            spool.check_session(session)?;
            let mut writer = NativeRows::new(raw.width, raw.height, raw.bit_depth, store.clone())
                .map_err(|e| e.to_string())?;
            let mut row = ScratchBytes::new(pool, raw.width as usize * 8, ROW_BYTES)
                .map_err(|e| e.to_string())?;
            let mut span =
                ScratchBytes::new(pool, ROW_BYTES, ROW_BYTES).map_err(|e| e.to_string())?;
            let identity = spool.bytes().payload_sha256();
            let mut cache = stream_chunks::ChunkCache::<128>::new(pool, identity, length as u64)?;
            let mut read_check = || guard();
            let mut work = 0usize;
            let mut spans = index.iter();
            for y in 0..raw.height {
                guard().map_err(|e| e.to_string())?;
                let mut x = 0;
                while x < raw.width {
                    let next = spans
                        .next()
                        .filter(|s| u32::from(s.y) == y && u32::from(s.x) == x)
                        .ok_or(PROTOCOL)?;
                    let bytes = usize::from(next.count) * pixel_bytes;
                    cache.copy_prefix(
                        identity,
                        u64::from(next.offset),
                        &mut span,
                        bytes,
                        // These are private, authenticated cache bytes. Check
                        // authority at the row and on each physical chunk read.
                        &|| Ok(()),
                        |start, count, chunk| {
                            work = work
                                .checked_add(count)
                                .filter(|&total| total <= length.saturating_mul(8))
                                .ok_or("JPEG XL span read work exceeds admission")?;
                            spool
                                .bytes()
                                .read_complete_chunk_into(start, count, chunk, &mut read_check)
                                .map_err(|e| e.to_string())
                        },
                    )?;
                    let end = x + u32::from(next.count);
                    let output = &mut row[x as usize * 8..end as usize * 8];
                    if raw.bit_depth == 8 {
                        for (word, value) in output.chunks_exact_mut(2).zip(&span[..bytes]) {
                            word.fill(*value);
                        }
                    } else {
                        for (word, value) in output
                            .chunks_exact_mut(2)
                            .zip(span[..bytes].chunks_exact(2))
                        {
                            word.copy_from_slice(&[value[1], value[0]]);
                        }
                    }
                    x = end;
                }
                writer
                    .row(y, &row, &mut || guard())
                    .map_err(|e| e.to_string())?;
            }
            if spans.next().is_some() {
                return Err(PROTOCOL.into());
            }
            writer.finish(&mut || guard()).map_err(|e| e.to_string())
        });
        match (output, spool.retire(&mut || session.cleanup_check())) {
            (Ok(output), Ok(())) => Ok((output, raw, orientation)),
            (Err(reason), Ok(())) => Err(reason),
            (Err(reason), Err(cleanup)) => Err(format!(
                "{reason}; JPEG XL span spool retirement refused: {cleanup}"
            )),
            (Ok(output), Err(reason)) => match output.abort(session, &|| session.cleanup_check()) {
                Ok(()) => Err(reason),
                Err(cleanup) => Err(format!(
                    "{reason}; JPEG XL span candidate retirement refused: {cleanup}"
                )),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::native::worker::tests::Fixture;
    use crate::raster::backed::Region;

    #[test]
    fn unordered_spans_verify_coverage_and_retire_bad_wire() {
        let root = Fixture::new();
        let (runtime, connection) = root.runtime_with_budgets(64 * 1024 * 1024, 64 * 1024 * 1024);
        let session = runtime.begin().unwrap();
        let base =
            NativeDocument::new_blank(2, 2, 16, &runtime.resident(), &runtime.disk(), &|| Ok(()))
                .unwrap();
        let mut bytes = session.byte_temporary(2, ByteRole::SourceBulk).unwrap();
        bytes.append(&[0xff, 0x0a], &mut || Ok(())).unwrap();
        let source = bytes.finish([19; 32], &mut || Ok(())).unwrap();
        let baseline = runtime.disk().used().unwrap();
        let display = Info {
            width: 3,
            height: 2,
            bit_depth: 16,
        };
        for fault in 0..6 {
            let mut wire = vec![if fault == 1 { 0 } else { 6 }];
            for y in (0u32..3).rev() {
                for x in (0u32..2).rev() {
                    let written_x = if fault == 2 && y == 0 { 0 } else { x };
                    for field in [written_x, y, 1] {
                        wire.extend_from_slice(&field.to_le_bytes());
                    }
                    for word in [12347 + x as u16, 23459 + y as u16, 34571, 45683] {
                        wire.extend_from_slice(&word.to_le_bytes());
                    }
                }
            }
            if fault == 3 {
                wire.pop();
            }
            if fault == 4 {
                wire.push(0);
            }
            let received = receive(
                &base,
                &session,
                &mut std::io::Cursor::new(wire),
                display,
                &|| Ok(()),
            );
            if fault == 5 {
                received.unwrap().abort(&session).unwrap();
                assert_eq!(runtime.disk().used().unwrap(), baseline);
                continue;
            }
            let result = received.and_then(|received| {
                received.stage(&base, &session, source.bytes().payload_sha256(), &|| Ok(()))
            });
            if fault == 0 {
                let (output, raw, orientation) = result.unwrap();
                assert_eq!(
                    raw,
                    Info {
                        width: 2,
                        height: 3,
                        bit_depth: 16
                    }
                );
                assert_eq!(orientation, Orientation::Rotate90);
                let pixels = output
                    .surface()
                    .prepare_region(Region::new([0, 0, 2, 3]).unwrap(), &mut || Ok(()))
                    .unwrap();
                assert_eq!(pixels.pixel16(1, 2).unwrap(), [12348, 23461, 34571, 45683]);
                drop(pixels);
                output.abort(&session, &|| session.cleanup_check()).unwrap();
            } else {
                assert!(result.is_err());
            }
            assert_eq!(runtime.disk().used().unwrap(), baseline);
        }
        source.retire(&mut || session.cleanup_check()).unwrap();
        drop((base, session, connection, runtime));
    }
}
