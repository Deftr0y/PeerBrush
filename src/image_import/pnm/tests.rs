use super::*;

pub(crate) fn fixture(kind: u8, width: u32, height: u32, channels: usize, max: u16) -> Vec<u8> {
    let header = match kind {
        1 | 4 => format!("P{kind}\n# bitmap\n{width} {height}\n"),
        2 | 3 | 5 | 6 => format!("P{kind}\n# samples\n{width} {height}\n{max}\n"),
        7 => {
            let tuple = match (channels, max) {
                (1, 1) => "BLACKANDWHITE",
                (2, 1) => "BLACKANDWHITE_ALPHA",
                (1, _) => "GRAYSCALE",
                (2, _) => "GRAYSCALE_ALPHA",
                (3, _) => "RGB",
                (4, _) => "RGB_ALPHA",
                _ => unreachable!(),
            };
            format!("P7\n# arbitrary\nWIDTH {width}\nHEIGHT {height}\nDEPTH {channels}\nMAXVAL {max}\nTUPLTYPE {tuple}\nENDHDR\n")
        }
        _ => unreachable!(),
    };
    let mut bytes = header.into_bytes();
    for y in 0..height {
        if kind == 4 {
            for x in (0..width).step_by(8) {
                let mut byte = 0;
                for bit in 0..8 {
                    if x + bit < width && (x + bit + y) % 3 == 0 {
                        byte |= 128 >> bit;
                    }
                }
                bytes.push(byte);
            }
            continue;
        }
        for x in 0..width {
            for c in 0..channels {
                let index = (y * width + x) as usize * channels + c;
                let value = match index % 7 {
                    0 => 0,
                    1 => 1,
                    2 => max,
                    3 => max.saturating_sub(1),
                    _ => ((index as u32 * 113) % (u32::from(max) + 1)) as u16,
                };
                if kind <= 3 {
                    bytes.extend_from_slice(format!("{value} ").as_bytes());
                } else if max > 255 {
                    bytes.extend_from_slice(&value.to_be_bytes());
                } else {
                    bytes.push(value as u8);
                }
            }
        }
        if kind <= 3 {
            bytes.push(b'\n');
        }
    }
    bytes
}

fn pixels(bytes: &[u8]) -> Result<(u16, Vec<u16>), String> {
    let mut input = Cursor::new(bytes);
    let header = Header::read(&mut input)?;
    let mut raw = vec![0; header.width as usize * usize::from(header.color.bytes_per_pixel())];
    let mut row = vec![0; header.width as usize * 8];
    let mut result = Vec::new();
    decode(
        &mut input,
        &header,
        &mut raw,
        &mut row,
        &|| Ok(()),
        |_, row| {
            result.extend(
                row.chunks_exact(2)
                    .map(|w| u16::from_be_bytes([w[0], w[1]])),
            );
            Ok(())
        },
    )?;
    Ok((header.depth(), result))
}

#[test]
fn streamed_netpbm_matches_installed_decoder_at_original_precision() {
    for kind in 1..=7 {
        let channels = match kind {
            3 | 6 => vec![3],
            7 => vec![1, 2, 3, 4],
            _ => vec![1],
        };
        let maxima = if kind == 1 || kind == 4 {
            vec![1]
        } else {
            vec![1, 15, 255, 1023, 65535]
        };
        for channels in channels {
            for &max in &maxima {
                for width in [1, 7, 259] {
                    let bytes = fixture(kind, width, 3, channels, max);
                    let expected =
                        image::load_from_memory_with_format(&bytes, image::ImageFormat::Pnm)
                            .unwrap()
                            .to_rgba16()
                            .into_raw();
                    let (depth, actual) = pixels(&bytes).unwrap();
                    assert_eq!(depth, if max > 255 { 16 } else { 8 });
                    assert_eq!(
                        actual, expected,
                        "P{kind}, {channels} channels, MAXVAL {max}, width {width}"
                    );
                }
            }
        }
    }
}

#[test]
fn malformed_headers_and_truncated_samples_refuse() {
    for bytes in [
        b"P5\n0 1\n255\n".as_slice(),
        b"P5\n16001 1\n255\n",
        b"P5\n1 1\n0\n",
        b"P7\nWIDTH 1\nHEIGHT 1\nDEPTH 5\nMAXVAL 255\nENDHDR\n",
    ] {
        assert!(pixels(bytes).is_err());
    }
    let mut capped = b"P5\n#".to_vec();
    capped.resize(HEADER_LIMIT as usize - 8, b'x');
    capped.extend_from_slice(b"\n1 1\n255");
    assert_eq!(capped.len(), HEADER_LIMIT as usize);
    assert!(pixels(&capped).unwrap_err().contains("64 KiB"));
    for kind in 1..=7 {
        let channels = if kind == 3 || kind == 6 { 3 } else { 1 };
        let max = if kind == 1 || kind == 4 { 1 } else { 65535 };
        let bytes = fixture(kind, 259, 3, channels, max);
        assert!(pixels(&bytes[..bytes.len() / 2]).is_err());
    }
}

#[test]
fn wide_and_tall_netpbm_rows_keep_packed_and_native_sample_values() {
    for (kind, channels, max) in [(4, 1, 1), (3, 3, 1023), (7, 4, 65535)] {
        for (width, height) in [(NATIVE_EDGE, 2), (2, NATIVE_EDGE)] {
            let bytes = fixture(kind, width, height, channels, max);
            let expected = image::load_from_memory_with_format(&bytes, image::ImageFormat::Pnm)
                .unwrap().to_rgba16().into_raw();
            let (depth, actual) = pixels(&bytes).unwrap();
            assert_eq!(depth, if max > 255 { 16 } else { 8 });
            assert_eq!(actual, expected);
        }
    }
    let header = b"P7\nWIDTH 16000\nHEIGHT 16000\nDEPTH 4\nMAXVAL 65535\nTUPLTYPE RGB_ALPHA\nENDHDR\n";
    let header = Header::read(&mut Cursor::new(header)).unwrap();
    assert_eq!((header.width, header.height, header.depth()), (16000, 16000, 16));
    assert!(FILE_LIMITS.encoded_bytes > u64::from(header.width) * u64::from(header.height) * 8);
}

#[test]
fn ascii_parser_cannot_swallow_last_sample_io_failure() {
    struct FailsAtEnd(Cursor<Vec<u8>>);
    impl Read for FailsAtEnd {
        fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
            let count = self.0.read(out)?;
            if count == 0 {
                Err(io::Error::other("sample read interrupted"))
            } else {
                Ok(count)
            }
        }
    }
    let failure = RefCell::new(None::<String>);
    let check = || failure.borrow().clone().map_or(Ok(()), Err);
    let mut input = Controlled {
        input: FailsAtEnd(Cursor::new(b"P2\n1 1\n255\n12".to_vec())),
        check: &check,
        failure: &failure,
    };
    let header = Header::read(&mut input).unwrap();
    let mut emitted = false;
    let reason = decode(
        &mut input,
        &header,
        &mut [0],
        &mut [0; 8],
        &check,
        |_, _| {
            emitted = true;
            Ok(())
        },
    )
    .unwrap_err();
    assert!(reason.contains("sample read interrupted"));
    assert!(!emitted);
}

#[test]
fn staged_netpbm_preserves_16_bit_words_and_retires_failed_output() {
    use crate::engine::native::worker::tests::Fixture;
    let root = Fixture::new();
    let (runtime, connection) = root.runtime_with_budgets(64 * 1024 * 1024, 64 * 1024 * 1024);
    let session = runtime.begin().unwrap();
    let base =
        NativeDocument::new_blank(4, 4, 16, &runtime.resident(), &runtime.disk(), &|| Ok(()))
            .unwrap();
    let path = root.path().join("source.PAM");
    let bytes = fixture(7, 259, 3, 4, 65535);
    std::fs::write(&path, &bytes).unwrap();
    let (_, expected) = pixels(&bytes).unwrap();
    let baseline = runtime.disk().used().unwrap();
    let output = prepare(&base, &session, &path, &|| Ok(())).unwrap();
    assert_eq!(output.surface().depth(), 16);
    for y in 0..3 {
        let row = output
            .surface()
            .prepare_region(
                crate::raster::backed::Region::new([0, y, 259, y + 1]).unwrap(),
                &mut || Ok(()),
            )
            .unwrap();
        for x in 0..259 {
            let at = (y as usize * 259 + x as usize) * 4;
            assert_eq!(
                row.pixel16(x, y).unwrap(),
                std::array::from_fn(|c| expected[at + c])
            );
        }
    }
    output.abort(&session, &|| session.cleanup_check()).unwrap();
    assert_eq!(runtime.disk().used().unwrap(), baseline);
    let reason = prepare(&base, &session, &path, &|| {
        if runtime.disk().used().unwrap() > baseline {
            Err("Netpbm interrupted".into())
        } else {
            Ok(())
        }
    })
    .err()
    .unwrap();
    assert!(reason.contains("Netpbm interrupted"), "{reason}");
    assert_eq!(runtime.disk().used().unwrap(), baseline);
    std::fs::write(&path, &bytes[..bytes.len() - 1]).unwrap();
    assert!(prepare(&base, &session, &path, &|| Ok(())).is_err());
    assert_eq!(runtime.disk().used().unwrap(), baseline);
    drop((base, session, runtime, connection));
}
