//! Parent-side bounded row reception through the existing isolated supervisor.
use super::*;
use crate::{
    engine::native::{NativeDocument, StagedOutput},
    large_document::ScratchBytes,
    selection::NativeRows,
    workspace::source_runtime::{SessionBytes, SourceSession},
};

const IO_BYTES: usize = 64 * 1024;
const MAX_ROW_BYTES: usize = crate::raster::NATIVE_EDGE as usize * 8;
const PARENT_BYTES: usize = 32 * 1024 * 1024;
// Same native callback and non-callback/CMS allowance as the reviewed encoder
// broker. Input, decoded output and returned profile are charged separately.
const NATIVE_BYTES: usize = 512 * 1024 * 1024;
const CHILD_BYTES: usize = 96 * 1024 * 1024;

#[must_use]
pub(crate) struct DecodedRows {
    pub output: StagedOutput,
    pub profile: ScratchBytes,
    pub info: Info,
    pub orientation: image::metadata::Orientation,
}

enum PendingRows<'session> {
    Complete(DecodedRows),
    Spans(spans::Received<'session>, ScratchBytes),
}

fn ownership(
    session: &SourceSession,
    output: Option<usize>,
    may_retire: Arc<AtomicBool>,
) -> Result<OperationOwnership, String> {
    let pool = session.resident();
    let headers = pool
        .reserve_bytes(65536, 65536)
        .map_err(|e| e.to_string())?;
    let mut charges = Vec::new();
    charges
        .try_reserve_exact(20)
        .map_err(|_| "JPEG XL reservation headers refused")?;
    for mut bytes in [
        if output.is_some() {
            NATIVE_BYTES
        } else {
            16 * 1024 * 1024
        },
        CHILD_BYTES,
        native::INPUT_WINDOW,
        output.unwrap_or(0),
        crate::color_profile::MAX_PROFILE_BYTES,
    ] {
        while bytes != 0 {
            let take = bytes.min(NATIVE_BUFFER_LIMIT);
            charges.push(
                pool.reserve_bytes(take, NATIVE_BUFFER_LIMIT)
                    .map_err(|e| e.to_string())?,
            );
            bytes -= take;
        }
    }
    Ok(OperationOwnership {
        source: session.retain_for_child()?,
        resources: ChildResources {
            charges,
            _headers: headers,
            _original_chunks: None,
        },
        may_retire,
    })
}

fn send_source(
    source: &SessionBytes<'_>,
    buffer: &mut ScratchBytes,
    pipe: &mut impl Write,
    stop: &AtomicBool,
) -> Result<(), String> {
    let check = || {
        if stop.load(Ordering::SeqCst) {
            return Err("JPEG XL source transfer cancelled".into());
        }
        source.check()
    };
    let mut failure = None;
    let mut io_check = || {
        check().map_err(|e| {
            failure = Some(e);
            crate::large_document::Error::Cancelled
        })
    };
    let result = (|| {
        let mut cursor = source
            .bytes()
            .cursor(0, source.bytes().payload_length(), &mut io_check)
            .map_err(|e| e.to_string())?;
        let mut remaining = source.bytes().payload_length();
        while remaining != 0 {
            check()?;
            let length = remaining.min(buffer.len() as u64) as usize;
            cursor
                .read_exact(&mut buffer[..length])
                .map_err(|e| e.to_string())?;
            write(pipe, &buffer[..length])?;
            remaining -= length as u64;
        }
        check()
    })();
    failure.map_or(result, Err)
}

fn inspect_source(
    source: &SessionBytes<'_>,
    session: &SourceSession,
    may_retire: Arc<AtomicBool>,
    check: &dyn Fn() -> Result<(), String>,
) -> Result<Info, String> {
    let length = source.bytes().payload_length() as usize;
    let owners = ownership(session, None, may_retire)?;
    let mut buffer =
        ScratchBytes::new(&session.resident(), IO_BYTES, IO_BYTES).map_err(|e| e.to_string())?;
    invoke_with(
        request(Operation::InspectNative, None, None, 0, length)?,
        move |pipe, stop| send_source(source, &mut buffer, pipe, &stop),
        |pipe, request, _| {
            read_output(
                pipe,
                request,
                #[cfg(test)]
                None,
            )
            .map(|out| out.info)
        },
        check,
        Runtime::default(),
        Some(owners),
    )
}

fn receive<'session>(
    base: &NativeDocument,
    session: &'session SourceSession,
    source: &SessionBytes<'_>,
    input: &mut impl Read,
    request: &Request,
    stop: &AtomicBool,
    held: &Mutex<Option<PendingRows<'session>>>,
) -> Result<(), String> {
    let check = || {
        if stop.load(Ordering::SeqCst) {
            return Err("JPEG XL row decoding cancelled".into());
        }
        source.check_session(session)
    };
    check()?;
    let response: Response = read_header(input)?;
    response.validate(request)?;
    if response.kind != Kind::Result {
        return Err(PROTOCOL.into());
    }
    if let Some(error) = response.error {
        eof(input)?;
        return Err(error.message());
    }
    let info = response.info.ok_or(PROTOCOL)?;
    let mut profile = ScratchBytes::new(
        base.budget(),
        response.profile_bytes as usize,
        crate::color_profile::MAX_PROFILE_BYTES,
    )
    .map_err(|e| e.to_string())?;
    read(input, &mut profile)?;
    crate::color_profile::supported(&profile)?;
    check()?;
    let pending = if request.operation == Operation::DecodeSpans {
        PendingRows::Spans(spans::receive(base, session, input, info, &check)?, profile)
    } else {
        let output = crate::retained::transaction::stage(base, session, &check, |store, guard| {
            let mut writer =
                NativeRows::new(info.width, info.height, info.bit_depth, store.clone())
                    .map_err(|e| e.to_string())?;
            let length = info.width as usize * 4 * usize::from(info.bit_depth / 8);
            let mut raw = ScratchBytes::new(base.budget(), length, MAX_ROW_BYTES)
                .map_err(|e| e.to_string())?;
            let mut row = ScratchBytes::new(base.budget(), info.width as usize * 8, MAX_ROW_BYTES)
                .map_err(|e| e.to_string())?;
            for y in 0..info.height {
                check()?;
                read(input, &mut raw)?;
                if info.bit_depth == 8 {
                    for (word, value) in row.chunks_exact_mut(2).zip(raw.iter()) {
                        word.fill(*value);
                    }
                } else {
                    for (word, value) in row.chunks_exact_mut(2).zip(raw.chunks_exact(2)) {
                        word.copy_from_slice(
                            &u16::from_le_bytes(value.try_into().unwrap()).to_be_bytes(),
                        );
                    }
                }
                writer
                    .row(y, &row, &mut || guard())
                    .map_err(|e| e.to_string())?;
            }
            writer.finish(&mut || guard()).map_err(|e| e.to_string())
        })?;
        PendingRows::Complete(DecodedRows {
            output,
            profile,
            info,
            orientation: image::metadata::Orientation::NoTransforms,
        })
    };
    *held.lock().unwrap_or_else(|e| e.into_inner()) = Some(pending);
    // Keep the completed owner visible to cleanup if EOF, exit, cancellation
    // or a late reservation check refuses after the final row was received.
    if request.operation != Operation::DecodeSpans {
        eof(input)?;
    }
    check()
}

pub(crate) fn decode(
    base: &NativeDocument,
    session: &SourceSession,
    source: &SessionBytes<'_>,
    patch_depth: Option<u16>,
    may_retire: Arc<AtomicBool>,
    check: &dyn Fn() -> Result<(), String>,
) -> Result<DecodedRows, String> {
    check()?;
    source.check_session(session)?;
    let length = usize::try_from(source.bytes().payload_length())
        .map_err(|_| "JPEG XL source size overflow")?;
    if !(2..=NATIVE_ENCODED_LIMIT).contains(&(length as u64)) {
        return Err("JPEG XL encoded source must be at most 3 GiB".into());
    }
    let _parent = base
        .budget()
        .reserve_bytes(PARENT_BYTES, PARENT_BYTES)
        .map_err(|e| e.to_string())?;
    let checked = || {
        check()?;
        source.check_session(session)
    };
    let info = inspect_source(source, session, may_retire.clone(), &checked)?;
    if let Some(depth) = patch_depth {
        crate::engine::image_patch_precision(info.bit_depth, depth)?;
    }
    let raw = image_bytes(info, CanvasLimits::Native)?;
    let (operation, contiguous) = if patch_depth.is_some() || raw > NATIVE_BUFFER_LIMIT {
        (Operation::DecodeSpans, 0)
    } else {
        (Operation::DecodeNative, raw)
    };
    let owners = ownership(session, Some(contiguous), may_retire.clone())?;
    let mut buffer =
        ScratchBytes::new(base.budget(), IO_BYTES, IO_BYTES).map_err(|e| e.to_string())?;
    let held = Mutex::new(None);
    let result = invoke_with(
        request(operation, Some(info), None, 0, length)?,
        move |pipe, stop| send_source(source, &mut buffer, pipe, &stop),
        |pipe, request, stop| receive(base, session, source, pipe, request, &stop, &held),
        &checked,
        Runtime::default(),
        Some(owners),
    )
    .and_then(|()| checked());
    let output = held.into_inner().unwrap_or_else(|e| e.into_inner());
    match (result, output) {
        (Ok(()), Some(PendingRows::Complete(output))) => Ok(output),
        (Ok(()), Some(PendingRows::Spans(received, profile))) => {
            let (output, info, orientation) =
                received.stage(base, session, source.bytes().payload_sha256(), &checked)?;
            Ok(DecodedRows {
                output,
                profile,
                info,
                orientation,
            })
        }
        (Ok(()), None) => Err(PROTOCOL.into()),
        (Err(reason), None) => Err(reason),
        (Err(reason), Some(output)) => {
            if !may_retire.load(Ordering::SeqCst) {
                return Err(format!(
                    "{reason}; JPEG XL staged rows retained for verified recovery"
                ));
            }
            let retirement = match output {
                PendingRows::Complete(output) => {
                    output.output.abort(session, &|| session.cleanup_check())
                }
                PendingRows::Spans(received, _) => received.abort(session),
            };
            match retirement {
                Ok(()) => Err(reason),
                Err(cleanup) => Err(format!(
                    "{reason}; JPEG XL output retirement refused: {cleanup}"
                )),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::native::worker::tests::Fixture;
    use crate::large_document::ByteRole;
    use crate::raster::backed::Region;
    use std::io::Cursor;

    #[test]
    fn actual_native_worker_preserves_wide_rgba16_rows() {
        let _codec = super::super::test_process_guard();
        let root = Fixture::new();
        let (runtime, connection) = root.runtime_with_budgets(768 * 1024 * 1024, 128 * 1024 * 1024);
        let session = runtime.begin().unwrap();
        let base =
            NativeDocument::new_blank(4, 4, 16, &runtime.resident(), &runtime.disk(), &|| Ok(()))
                .unwrap();
        let edge = crate::raster::NATIVE_EDGE;
        let pixel = [12347, 23459, 34571, 45683];
        let words = pixel.repeat(edge as usize * 3);
        let encoded = native::encode_buffer(
            edge,
            3,
            PixelsRef::Sixteen(&words),
            None,
            Options::default(),
            CanvasLimits::Native,
            &|| Ok(()),
        )
        .unwrap();
        assert!(super::super::inspect(&encoded, &|| Ok(())).is_err());
        let path = root.path().join("Wide source.JXL");
        std::fs::write(&path, &encoded).unwrap();
        let info = crate::image_import::inspect(&path).unwrap();
        assert_eq!(info.format, "JPEG XL");
        assert_eq!((info.width, info.height, info.bit_depth), (edge, 3, 16));
        let disk = runtime.disk().used().unwrap();
        let output = crate::image_import::jxl::prepare(
            &base,
            &session,
            &path,
            &serde_json::json!({}),
            &|| Ok(()),
        )
        .unwrap();
        let surface = output.surface();
        assert_eq!(
            (surface.width(), surface.height(), surface.depth()),
            (edge, 3, 16)
        );
        let x = edge as i32 - 1;
        let region = Region::new([x, 2, x + 1, 3]).unwrap();
        assert_eq!(
            surface
                .prepare_region(region, &mut || Ok(()))
                .unwrap()
                .pixel16(x, 2)
                .unwrap(),
            pixel
        );
        output.abort(&session, &|| session.cleanup_check()).unwrap();
        assert_eq!(runtime.disk().used().unwrap(), disk);
        assert_eq!(std::fs::read(&path).unwrap(), encoded);
        drop((base, session, connection, runtime));
    }

    #[test]
    fn native_protocol_checks_the_bound_before_accepting_a_decode_request() {
        let info = Info {
            width: crate::raster::NATIVE_EDGE,
            height: 3,
            bit_depth: 16,
        };
        let admitted = request(Operation::DecodeNative, Some(info), None, 0, 2).unwrap();
        assert!(admitted.validate().is_ok());
        for invalid in [
            None,
            Some(Info {
                width: info.width + 1,
                ..info
            }),
            Some(Info {
                height: info.width,
                ..info
            }),
            Some(Info {
                bit_depth: 12,
                ..info
            }),
        ] {
            assert!(request(Operation::DecodeNative, invalid, None, 0, 2)
                .unwrap()
                .validate()
                .is_err());
        }
        assert!(request(Operation::Decode, Some(info), None, 0, 2)
            .unwrap()
            .validate()
            .is_err());
        let inspection = request(Operation::InspectNative, None, None, 0, 2).unwrap();
        let mut response = inspection.response(Kind::Result);
        response.info = Some(Info {
            height: info.width,
            ..info
        });
        assert!(response.validate(&inspection).is_ok());
        assert!(request(Operation::DecodeSpans, response.info, None, 0, 2)
            .unwrap()
            .validate()
            .is_ok());
        for (operation, expected) in [(Operation::InspectNative, None), (Operation::DecodeSpans, response.info)] {
            assert!(request(operation, expected, None, 0, NATIVE_ENCODED_LIMIT as usize)
                .unwrap().validate().is_ok());
            assert!(request(operation, expected, None, 0, NATIVE_ENCODED_LIMIT as usize + 1)
                .unwrap().validate().is_err());
        }
        response.payload_bytes = 1;
        assert!(response.validate(&inspection).is_err());
    }

    #[test]
    fn actual_worker_sends_partial_scanlines_without_a_registered_frame_buffer() {
        let _codec = super::super::test_process_guard();
        let root = Fixture::new();
        let (runtime, connection) = root.runtime_with_budgets(768 * 1024 * 1024, 128 * 1024 * 1024);
        let session = runtime.begin().unwrap();
        let base =
            NativeDocument::new_blank(4, 4, 16, &runtime.resident(), &runtime.disk(), &|| Ok(()))
                .unwrap();
        let info = Info {
            width: 1057,
            height: 259,
            bit_depth: 16,
        };
        let sample = |x: u32, y: u32| {
            [
                (x * 199 + y * 113) as u16,
                (x * 509 + y * 97) as u16,
                (x * 137 + y * 1597) as u16,
                65535,
            ]
        };
        let words: Vec<_> = (0..info.height)
            .flat_map(|y| (0..info.width).flat_map(move |x| sample(x, y)))
            .collect();
        let encoded = native::encode_buffer(
            info.width,
            info.height,
            PixelsRef::Sixteen(&words),
            None,
            Options::default(),
            CanvasLimits::Native,
            &|| Ok(()),
        )
        .unwrap();
        let mut bytes = session
            .byte_temporary(encoded.len() as u64, ByteRole::SourceBulk)
            .unwrap();
        bytes.append(&encoded, &mut || Ok(())).unwrap();
        let source = bytes.finish([19; 32], &mut || Ok(())).unwrap();
        let disk = runtime.disk().used().unwrap();
        let held = Mutex::new(None);
        let stop = Arc::new(AtomicBool::new(true));
        let owners = ownership(&session, Some(0), stop).unwrap();
        let mut transfer = ScratchBytes::new(base.budget(), IO_BYTES, IO_BYTES).unwrap();
        invoke_with(
            request(Operation::DecodeSpans, Some(info), None, 0, encoded.len()).unwrap(),
            |pipe, stop| send_source(&source, &mut transfer, pipe, &stop),
            |pipe, request, stop| receive(&base, &session, &source, pipe, request, &stop, &held),
            &|| Ok(()),
            Runtime::default(),
            Some(owners),
        )
        .unwrap();
        let PendingRows::Spans(received, _profile) = held.into_inner().unwrap().unwrap() else {
            panic!("Span worker must return a provisional spool before native tile replay");
        };
        let (output, decoded, _) = received
            .stage(&base, &session, source.bytes().payload_sha256(), &|| Ok(()))
            .unwrap();
        assert_eq!(decoded, info);
        for (x, y) in [(0, 0), (256, 255), (1056, 258)] {
            let region = Region::new([x as i32, y as i32, x as i32 + 1, y as i32 + 1]).unwrap();
            let pixels = output
                .surface()
                .prepare_region(region, &mut || Ok(()))
                .unwrap();
            assert_eq!(pixels.pixel16(x as i32, y as i32).unwrap(), sample(x, y));
        }
        output.abort(&session, &|| session.cleanup_check()).unwrap();
        assert_eq!(runtime.disk().used().unwrap(), disk);
        source.retire(&mut || session.cleanup_check()).unwrap();
        drop((base, session, runtime, connection));
    }

    #[test]
    fn row_receiver_preserves_words_and_retires_truncated_or_trailing_results() {
        let root = Fixture::new();
        let (runtime, connection) = root.runtime_with_budgets(128 * 1024 * 1024, 128 * 1024 * 1024);
        let session = runtime.begin().unwrap();
        let base =
            NativeDocument::new_blank(4, 4, 16, &runtime.resident(), &runtime.disk(), &|| Ok(()))
                .unwrap();
        let mut bytes = session.byte_temporary(2, ByteRole::SourceBulk).unwrap();
        bytes.append(&[0xff, 0x0a], &mut || Ok(())).unwrap();
        let source = bytes.finish([19; 32], &mut || Ok(())).unwrap();
        let baseline = runtime.disk().used().unwrap();
        let resident = runtime.resident().used();
        for depth in [8, 16] {
            let info = Info {
                width: 270,
                height: 3,
                bit_depth: depth,
            };
            let request = request(Operation::DecodeNative, Some(info), None, 0, 2).unwrap();
            let profile = crate::color_profile::srgb_profile();
            let words: Vec<u16> = (0..270 * 3 * 4)
                .map(|n| {
                    if depth == 8 {
                        (n as u8) as u16 * 257
                    } else {
                        (n * 1009 + 12347) as u16
                    }
                })
                .collect();
            for fault in 0..4 {
                let mut response = request.response(Kind::Result);
                response.info = Some(if fault == 3 {
                    Info {
                        bit_depth: 24 - depth,
                        ..info
                    }
                } else {
                    info
                });
                response.profile_bytes = profile.len() as u32;
                response.payload_bytes = raw_size(info).unwrap() as u64;
                let mut wire = Vec::new();
                header(&mut wire, &response).unwrap();
                wire.extend_from_slice(profile);
                for word in &words {
                    if depth == 8 {
                        wire.push((word / 257) as u8);
                    } else {
                        wire.extend(word.to_le_bytes());
                    }
                }
                if fault == 1 {
                    wire.pop();
                }
                if fault == 2 {
                    wire.push(0);
                }
                let held = Mutex::new(None);
                let result = receive(
                    &base,
                    &session,
                    &source,
                    &mut Cursor::new(wire),
                    &request,
                    &AtomicBool::new(false),
                    &held,
                );
                assert_eq!(result.is_ok(), fault == 0, "{result:?}");
                if let Some(pending) = held.into_inner().unwrap() {
                    let PendingRows::Complete(decoded) = pending else {
                        panic!("Contiguous receiver must stage complete rows");
                    };
                    if fault == 0 {
                        assert_eq!(decoded.info, info);
                        let pixels = decoded
                            .output
                            .surface()
                            .prepare_region(Region::new([0, 0, 270, 3]).unwrap(), &mut || Ok(()))
                            .unwrap();
                        for (x, y) in [(0, 0), (256, 1), (269, 2)] {
                            let at = (y * 270 + x) * 4;
                            assert_eq!(
                                pixels.pixel16(x as i32, y as i32).unwrap(),
                                words[at..at + 4]
                            );
                        }
                    }
                    decoded
                        .output
                        .abort(&session, &|| session.cleanup_check())
                        .unwrap();
                }
                assert_eq!(runtime.disk().used().unwrap(), baseline);
                assert_eq!(runtime.resident().used(), resident);
            }
        }
        source.retire(&mut || session.cleanup_check()).unwrap();
        drop((base, session, connection, runtime));
    }
}
