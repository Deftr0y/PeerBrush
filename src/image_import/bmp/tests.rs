use super::*;

pub(crate) fn fixture(
    width: u32,
    height: u32,
    dib: usize,
    bits: usize,
    compression: u32,
    top_down: bool,
    alpha: bool,
) -> Vec<u8> {
    let colors = if bits <= 8 { 1 << bits } else { 0 };
    let extra = usize::from(dib == 40 && compression == 3) * 12;
    let palette_stride = if dib == 12 { 3 } else { 4 };
    let prefix = 14 + dib + extra + colors * palette_stride;
    let offset = prefix + 13;
    let stride = (width as usize * bits).div_ceil(32) * 4;
    let size = offset + height as usize * stride;
    let mut bytes = vec![0; size];
    bytes[..2].copy_from_slice(b"BM");
    bytes[2..6].copy_from_slice(&(size as u32).to_le_bytes());
    bytes[10..14].copy_from_slice(&(offset as u32).to_le_bytes());
    bytes[14..18].copy_from_slice(&(dib as u32).to_le_bytes());
    if dib == 12 {
        bytes[18..20].copy_from_slice(&(width as u16).to_le_bytes());
        bytes[20..22].copy_from_slice(&(height as u16).to_le_bytes());
        bytes[22..24].copy_from_slice(&1u16.to_le_bytes());
        bytes[24..26].copy_from_slice(&(bits as u16).to_le_bytes());
    } else {
        bytes[18..22].copy_from_slice(&width.to_le_bytes());
        bytes[22..26].copy_from_slice(
            &(if top_down {
                -(height as i32)
            } else {
                height as i32
            })
            .to_le_bytes(),
        );
        bytes[26..28].copy_from_slice(&1u16.to_le_bytes());
        bytes[28..30].copy_from_slice(&(bits as u16).to_le_bytes());
        bytes[30..34].copy_from_slice(&compression.to_le_bytes());
        bytes[34..38].copy_from_slice(&((height as usize * stride) as u32).to_le_bytes());
        if compression == 3 {
            let masks: [u32; 4] = if bits == 16 {
                if alpha {
                    [0x0f00, 0x00f0, 0x000f, 0xf000]
                } else {
                    [0xf800, 0x07e0, 0x001f, 0]
                }
            } else {
                [
                    0x00ff0000,
                    0x0000ff00,
                    0x000000ff,
                    if alpha { 0xff000000 } else { 0 },
                ]
            };
            for (i, mask) in masks[..if dib >= 56 { 4 } else { 3 }].iter().enumerate() {
                bytes[54 + i * 4..58 + i * 4].copy_from_slice(&mask.to_le_bytes());
            }
        }
    }
    for i in 0..colors {
        let at = 14 + dib + extra + i * palette_stride;
        bytes[at..at + 3].copy_from_slice(&[
            (13 + i * 17) as u8,
            (29 + i * 31) as u8,
            (47 + i * 7) as u8,
        ]);
    }
    for y in 0..height as usize {
        let row = &mut bytes[offset + y * stride..offset + (y + 1) * stride];
        for x in 0..width as usize {
            let seed = x + y * 19;
            match bits {
                1 | 2 | 4 | 8 => {
                    row[x * bits / 8] |= ((seed % colors) << (8 - bits - x * bits % 8)) as u8
                }
                16 => row[x * 2..x * 2 + 2].copy_from_slice(&((seed * 2731) as u16).to_le_bytes()),
                24 => row[x * 3..x * 3 + 3].copy_from_slice(&[
                    (seed * 17) as u8,
                    (seed * 31) as u8,
                    (seed * 7) as u8,
                ]),
                32 => row[x * 4..x * 4 + 4].copy_from_slice(&[
                    (seed * 17) as u8,
                    (seed * 31) as u8,
                    (seed * 7) as u8,
                    if seed % 5 == 0 { 0 } else { 193 },
                ]),
                _ => unreachable!(),
            }
        }
    }
    bytes
}

fn payload(mut bytes: Vec<u8>, data: &[u8]) -> Vec<u8> {
    let offset = u32::from_le_bytes(bytes[10..14].try_into().unwrap()) as usize;
    bytes.truncate(offset);
    bytes.extend_from_slice(data);
    let size = bytes.len() as u32;
    bytes[2..6].copy_from_slice(&size.to_le_bytes());
    bytes[34..38].copy_from_slice(&(data.len() as u32).to_le_bytes());
    bytes
}
pub(crate) fn rle_fixture(bits: usize) -> Vec<u8> {
    let mut data = vec![2, if bits == 4 { 0x12 } else { 1 }, 0, 2, 1, 2, 0, 3];
    data.extend_from_slice(if bits == 4 {
        &[0x34, 0x50]
    } else {
        &[3, 4, 5, 0]
    });
    data.extend_from_slice(&[
        0,
        2,
        1,
        0,
        2,
        if bits == 4 { 0x67 } else { 6 },
        0,
        0,
        0,
        0,
        0,
        1,
    ]);
    payload(
        fixture(9, 5, 40, bits, if bits == 4 { 2 } else { 1 }, false, false),
        &data,
    )
}
fn pixels(bytes: &[u8]) -> Result<Vec<u8>, String> {
    let pool = ResidentBudget::new(2 * 1024 * 1024).unwrap();
    let _metadata = pool.reserve_bytes(METADATA_BYTES, METADATA_BYTES).unwrap();
    let mut input = Cursor::new(bytes);
    let header = Header::read(&mut input)?;
    let mut result = vec![0; header.width as usize * header.height as usize * 4];
    decode(&mut input, &header, &pool, &|| Ok(()), |y, row| {
        let y = if header.top_down {
            y
        } else {
            header.height - 1 - y
        };
        let at = y as usize * header.width as usize * 4;
        for (out, word) in result[at..at + header.width as usize * 4]
            .iter_mut()
            .zip(row.chunks_exact(2))
        {
            let word = u16::from_be_bytes([word[0], word[1]]);
            assert_eq!(word % 257, 0);
            *out = (word / 257) as u8;
        }
        Ok(())
    })?;
    assert_eq!(pool.used(), METADATA_BYTES);
    Ok(result)
}
fn compare(bytes: &[u8]) {
    let expected = image::load_from_memory_with_format(bytes, image::ImageFormat::Bmp)
        .unwrap()
        .to_rgba8()
        .into_raw();
    assert_eq!(pixels(bytes).unwrap(), expected);
}

pub(crate) fn wide_channels() -> Vec<u8> {
    let mut bytes = fixture(1, 2, 56, 32, 3, false, true);
    for (i, mask) in [0x3ff00000u32, 0x000ffc00, 0x000003ff, 0xc0000000]
        .iter()
        .enumerate()
    {
        bytes[54 + i * 4..58 + i * 4].copy_from_slice(&mask.to_le_bytes());
    }
    bytes
}

#[test]
fn bmp_and_dib_preflight_refuse_silent_bitfield_truncation_and_ignored_color_profiles() {
    let mut profiled = fixture(1, 2, 124, 32, 3, false, true);
    profiled[70..74].copy_from_slice(&0x4d424544u32.to_le_bytes());
    for bytes in [wide_channels(), profiled] {
        assert!(pixels(&bytes).is_err());
        assert!(validate_dib(&bytes[14..]).is_err());
        assert!(check_reader(&mut Cursor::new(&bytes), image::ImageFormat::Bmp).is_err());
        assert!(super::super::Encoded::new(bytes, image::ImageFormat::Bmp).is_err());
    }
}

#[test]
fn icon_preflight_checks_the_same_variant_as_the_installed_decoder_and_explicit_import() {
    let dib = |bytes: Vec<u8>| {
        let offset = u32::from_le_bytes(bytes[10..14].try_into().unwrap()) as usize;
        [bytes[14..70].to_vec(), bytes[offset..].to_vec()].concat()
    };
    let wide = dib(wide_channels());
    let narrow = dib(fixture(1, 2, 56, 32, 3, false, true));
    let icon = |entries: &[(u16, &[u8])]| {
        let mut bytes = vec![0; 6 + 16 * entries.len()];
        bytes[2..4].copy_from_slice(&1u16.to_le_bytes());
        bytes[4..6].copy_from_slice(&(entries.len() as u16).to_le_bytes());
        for (i, (bits, dib)) in entries.iter().enumerate() {
            let offset = bytes.len() as u32;
            let entry = &mut bytes[6 + i * 16..22 + i * 16];
            entry[0] = 1;
            entry[1] = 1;
            entry[4..6].copy_from_slice(&1u16.to_le_bytes());
            entry[6..8].copy_from_slice(&bits.to_le_bytes());
            entry[8..12].copy_from_slice(&(dib.len() as u32).to_le_bytes());
            entry[12..16].copy_from_slice(&offset.to_le_bytes());
            bytes.extend_from_slice(dib);
        }
        bytes
    };
    for entries in [
        vec![(32, &wide[..]), (24, &narrow[..])],
        vec![(24, &narrow[..]), (32, &wide[..])],
        vec![(32, &wide[..]), (32, &narrow[..]), (24, &narrow[..])],
    ] {
        assert!(check_reader(&mut Cursor::new(icon(&entries)), image::ImageFormat::Ico).is_err());
    }
    let bytes = icon(&[(32, &wide), (32, &narrow)]);
    let mut reader = Cursor::new(&bytes);
    check_reader(&mut reader, image::ImageFormat::Ico).unwrap();
    assert_eq!(reader.position(), 0);
    let encoded = super::super::Encoded::new(bytes, image::ImageFormat::Ico).unwrap();
    assert!(encoded
        .decode(&serde_json::json!({"variant":0}), super::super::BUDGET)
        .is_err());
    assert!(encoded
        .decode(&serde_json::json!({"variant":1}), super::super::BUDGET)
        .is_ok());
}

#[test]
fn bmp_rows_match_installed_decoder_for_core_info_palettes_masks_and_orientation() {
    for (dib, bits, compression, alpha) in [
        (12, 1, 0, false),
        (12, 4, 0, false),
        (12, 8, 0, false),
        (12, 24, 0, false),
        (40, 1, 0, false),
        (40, 2, 0, false),
        (40, 4, 0, false),
        (40, 8, 0, false),
        (40, 16, 0, false),
        (40, 24, 0, false),
        (40, 32, 0, false),
        (40, 16, 3, false),
        (40, 32, 3, false),
        (52, 16, 3, false),
        (56, 16, 3, true),
        (56, 32, 3, true),
        (108, 32, 3, true),
        (124, 16, 3, true),
    ] {
        for top_down in [false, true] {
            if dib == 12 && top_down {
                continue;
            }
            for width in [1, 7, 259] {
                compare(&fixture(width, 3, dib, bits, compression, top_down, alpha));
            }
        }
    }
    compare(&fixture(NATIVE_EDGE, 1, 124, 32, 3, false, true));
    compare(&fixture(1, NATIVE_EDGE, 40, 24, 0, true, false));
}

#[test]
fn bmp_rle_deltas_absolute_padding_and_early_end_match_black_gap_semantics() {
    for bits in [4, 8] {
        compare(&rle_fixture(bits));
        compare(&payload(
            fixture(NATIVE_EDGE, 1, 40, bits, if bits == 4 { 2 } else { 1 }, false, false),
            &[1, 0x11, 0, 1],
        ));
        for count in [3usize, 5, 6, 255] {
            let mut data = vec![0, count as u8];
            let bytes = if bits == 4 { count.div_ceil(2) } else { count };
            for i in 0..bytes {
                data.push((i * 17 + 31) as u8);
            }
            if bytes % 2 != 0 {
                data.push(0);
            }
            data.extend_from_slice(&[0, 0, 0, 1]);
            compare(&payload(
                fixture(
                    259,
                    3,
                    40,
                    bits,
                    if bits == 4 { 2 } else { 1 },
                    false,
                    false,
                ),
                &data,
            ));
        }
        // A zero-length delta does not create a row or change the cursor.
        compare(&payload(
            fixture(9, 3, 40, bits, if bits == 4 { 2 } else { 1 }, false, false),
            &[0, 2, 0, 0, 0, 1],
        ));
    }
}

#[test]
fn bmp_rejects_bad_metadata_truncation_and_out_of_frame_rle() {
    let valid = fixture(7, 3, 56, 16, 3, false, true);
    for at in [0, 17, 40, valid.len() - 1] {
        assert!(pixels(&valid[..at]).is_err());
    }
    for (at, value) in [(0, b'X'), (14, 64), (26, 2), (28, 12), (30, 4)] {
        let mut bad = valid.clone();
        bad[at] = value;
        assert!(pixels(&bad).is_err());
    }
    for offset in [0u32, 20, FILE_LIMITS.encoded_bytes as u32] {
        let mut bad = valid.clone();
        bad[10..14].copy_from_slice(&offset.to_le_bytes());
        assert!(pixels(&bad).is_err());
    }
    for width in [0u32, NATIVE_EDGE + 1, u32::MAX] {
        let mut bad = valid.clone();
        bad[18..22].copy_from_slice(&width.to_le_bytes());
        assert!(Header::read(&mut Cursor::new(&bad)).is_err());
    }
    let header = Header::read(&mut Cursor::new(&valid)).unwrap();
    assert!(header.check_extent(valid.len() as u64).is_ok());
    assert!(header.check_extent(valid.len() as u64 - 1).is_err());
    for bits in [4, 8] {
        let valid = rle_fixture(bits);
        assert!(pixels(&valid[..valid.len() - 1]).is_err());
        for data in [&[10, 1][..], &[0, 2, 0, 5], &[0, 2, 10, 0], &[0, 3, 1]] {
            assert!(pixels(&payload(valid.clone(), data)).is_err());
        }
    }
}

#[test]
fn bmp_rle_checks_cancellation_during_packets_that_leave_the_row_unchanged() {
    let header = Header::read(&mut Cursor::new(rle_fixture(8))).unwrap();
    let mut input = Cursor::new([0u8, 2, 0, 0].repeat(300));
    let mut row = vec![0; header.width as usize * 8];
    let checks = std::cell::Cell::new(0);
    let error = rle(
        &mut input,
        &header,
        &mut row,
        &|| {
            checks.set(checks.get() + 1);
            if checks.get() == 2 {
                Err("BMP cancelled".into())
            } else {
                Ok(())
            }
        },
        |_, _| unreachable!("A zero delta cannot emit a row"),
    )
    .unwrap_err();
    assert_eq!(error, "BMP cancelled");
    assert_eq!(input.position(), 256 * 4);
}

#[test]
fn native_bmp_preserves_oriented_alpha_and_retires_cancelled_fragments() {
    use crate::engine::native::worker::tests::Fixture;
    let root = Fixture::new();
    let (runtime, connection) = root.runtime_with_budgets(64 * 1024 * 1024, 64 * 1024 * 1024);
    let session = runtime.begin().unwrap();
    let base =
        NativeDocument::new_blank(4, 4, 16, &runtime.resident(), &runtime.disk(), &|| Ok(()))
            .unwrap();
    let path = root.path().join("source.BMP");
    let bytes = fixture(259, 259, 56, 32, 3, false, true);
    std::fs::write(&path, &bytes).unwrap();
    let expected = image::load_from_memory_with_format(&bytes, image::ImageFormat::Bmp)
        .unwrap()
        .to_rgba8()
        .into_raw();
    let baseline = runtime.disk().used().unwrap();
    let peak = std::cell::Cell::new(baseline);
    let output = prepare(&base, &session, &path, &|| {
        peak.set(peak.get().max(runtime.disk().used().unwrap()));
        Ok(())
    })
    .unwrap();
    assert_eq!(output.surface().depth(), 8);
    assert_eq!(peak.get(), runtime.disk().used().unwrap());
    for y in [0, 1, 255, 256, 258] {
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
                std::array::from_fn(|c| u16::from(expected[at + c]) * 257)
            );
        }
    }
    output.abort(&session, &|| session.cleanup_check()).unwrap();
    assert_eq!(runtime.disk().used().unwrap(), baseline);
    for threshold in [baseline + 1, peak.get()] {
        let reason = prepare(&base, &session, &path, &|| {
            if runtime.disk().used().unwrap() >= threshold {
                Err("BMP interrupted".into())
            } else {
                Ok(())
            }
        })
        .err()
        .unwrap();
        assert!(reason.contains("BMP interrupted"), "{reason}");
        assert_eq!(runtime.disk().used().unwrap(), baseline);
    }
    std::fs::write(&path, &bytes[..bytes.len() - 1]).unwrap();
    assert!(prepare(&base, &session, &path, &|| Ok(())).is_err());
    assert_eq!(runtime.disk().used().unwrap(), baseline);
    drop((base, session, runtime, connection));
}
