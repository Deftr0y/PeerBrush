use super::*;

pub(crate) fn fixture(
    width: u16,
    height: u16,
    kind: u8,
    bits: u8,
    palette_bits: u8,
    orientation: u8,
) -> Vec<u8> {
    let mapped = matches!(kind, 1 | 9);
    let gray = matches!(kind, 3 | 11);
    let origin: u16 = if bits == 16 { 300 } else { 3 };
    let mut header = [0; 18];
    header[0] = 3;
    header[1] = u8::from(mapped);
    header[2] = kind;
    if mapped {
        header[3..5].copy_from_slice(&origin.to_le_bytes());
        header[5..7].copy_from_slice(&6u16.to_le_bytes());
        header[7] = palette_bits;
    }
    header[8..12].copy_from_slice(&[7, 0, 11, 0]);
    header[12..14].copy_from_slice(&width.to_le_bytes());
    header[14..16].copy_from_slice(&height.to_le_bytes());
    header[16] = bits;
    let color_bits = if mapped { palette_bits } else { bits };
    let alpha = if color_bits == 32 || (gray && bits == 16) {
        8
    } else if color_bits == 16 {
        1
    } else {
        0
    };
    header[17] = orientation | alpha;
    let sample = |i: u32, bits: u8, gray: bool| -> Vec<u8> {
        if !gray && matches!(bits, 15 | 16) {
            let packed =
                ((i * 7 & 31) | ((i * 11 & 31) << 5) | ((i * 17 & 31) << 10) | ((i % 2) << 15))
                    as u16;
            packed.to_le_bytes().to_vec()
        } else if gray {
            [(i * 37) as u8, if i % 5 == 0 { 0 } else { 173 }][..usize::from(bits / 8)].to_vec()
        } else {
            [
                (i * 17) as u8,
                (i * 31) as u8,
                (i * 7) as u8,
                if i % 5 == 0 { 0 } else { 193 },
            ][..usize::from(bits / 8)]
                .to_vec()
        }
    };
    let mut bytes = header.to_vec();
    bytes.extend_from_slice(b"tag");
    if mapped {
        for i in 0..6 {
            bytes.extend(sample(i, palette_bits, false));
        }
    }
    let pixel = |i: u32| {
        if mapped {
            let index = origin + (i % 6) as u16;
            index.to_le_bytes()[..usize::from(bits / 8)].to_vec()
        } else {
            sample(i, bits, gray)
        }
    };
    let total = u32::from(width) * u32::from(height);
    let mut at = 0;
    let mut packet = 0;
    while at < total {
        if kind < 9 {
            bytes.extend(pixel(at));
            at += 1;
        } else {
            let repeat = packet % 2 == 0;
            let count = (total - at).min(if repeat { 5 } else { 17 });
            bytes.push((count - 1) as u8 | if repeat { 128 } else { 0 });
            if repeat {
                bytes.extend(pixel(at));
            } else {
                for i in at..at + count {
                    bytes.extend(pixel(i));
                }
            }
            at += count;
            packet += 1;
        }
    }
    bytes
}

fn pixels(bytes: &[u8]) -> Result<Vec<u8>, String> {
    let pool = ResidentBudget::new(2 * 1024 * 1024).unwrap();
    let mut input = Cursor::new(bytes);
    let header = Header::read(&mut input)?;
    let mut result = vec![0; header.width as usize * header.height as usize * 4];
    decode(&mut input, &header, &pool, &|| Ok(()), |y, row| {
        let y = if header.bytes[17] & 0x20 != 0 {
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
    assert_eq!(pool.used(), 0);
    Ok(result)
}

#[test]
fn tga_stream_matches_installed_decoder_for_packets_palettes_channels_and_origins() {
    for (kind, bits, palette) in [
        (1, 8, 15),
        (1, 16, 16),
        (1, 8, 24),
        (1, 16, 32),
        (9, 8, 15),
        (9, 16, 16),
        (9, 8, 24),
        (9, 16, 32),
        (2, 15, 0),
        (2, 16, 0),
        (2, 24, 0),
        (2, 32, 0),
        (10, 15, 0),
        (10, 16, 0),
        (10, 24, 0),
        (10, 32, 0),
        (3, 8, 0),
        (3, 16, 0),
        (11, 8, 0),
        (11, 16, 0),
    ] {
        for orientation in [0, 0x10, 0x20, 0x30] {
            for width in [1, 7, 259] {
                let bytes = fixture(width, 3, kind, bits, palette, orientation);
                let expected = image::load_from_memory_with_format(&bytes, image::ImageFormat::Tga)
                    .unwrap()
                    .to_rgba8()
                    .into_raw();
                assert_eq!(pixels(&bytes).unwrap(), expected, "type {kind}, bits {bits}, palette {palette}, origin {orientation}, width {width}");
            }
        }
    }
    let mut alpha_only = fixture(7, 3, 3, 8, 0, 0x10);
    alpha_only[17] |= 8;
    let mut unmarked_alpha = fixture(7, 3, 10, 32, 0, 0);
    unmarked_alpha[17] &= !15;
    let mut unused_palette = fixture(7, 3, 2, 24, 0, 0);
    unused_palette[1] = 1;
    unused_palette[5] = 2;
    unused_palette[7] = 16;
    unused_palette.splice(21..21, [1, 2, 3, 4]);
    for bytes in [
        alpha_only,
        unmarked_alpha,
        unused_palette,
        fixture(NATIVE_EDGE as u16, 3, 10, 32, 0, 0x10),
        fixture(3, NATIVE_EDGE as u16, 9, 16, 32, 0x30),
        fixture(NATIVE_EDGE as u16, 1, 2, 24, 0, 0x20),
    ] {
        let expected = image::load_from_memory_with_format(&bytes, image::ImageFormat::Tga)
            .unwrap()
            .to_rgba8()
            .into_raw();
        assert_eq!(pixels(&bytes).unwrap(), expected);
    }
}

#[test]
fn tga_refuses_invalid_indices_truncation_overlong_packets_and_interleaving() {
    let valid = fixture(259, 3, 9, 16, 32, 0);
    for at in [0, 17, 20, 35, valid.len() / 2, valid.len() - 1] {
        assert!(pixels(&valid[..at]).is_err());
    }
    for (at, value) in [(1, 2), (2, 0), (7, 8), (16, 32), (17, 0x48)] {
        let mut bad = valid.clone();
        bad[at] = value;
        assert!(pixels(&bad).is_err());
    }
    let mut bad_index = fixture(7, 3, 1, 8, 24, 0);
    bad_index[21 + 6 * 3] = 2;
    assert!(pixels(&bad_index).unwrap_err().contains("color map index"));
    let mut overlong = fixture(1, 1, 10, 32, 0, 0x20);
    overlong[21] = 0xff;
    assert!(pixels(&overlong).unwrap_err().contains("exceeds"));
    let mut raw_overlong = overlong;
    raw_overlong[21] = 0x7f;
    assert!(pixels(&raw_overlong).unwrap_err().contains("exceeds"));
    for width in [0, NATIVE_EDGE as u16 + 1, u16::MAX] {
        let mut bad = valid.clone();
        bad[12..14].copy_from_slice(&width.to_le_bytes());
        assert!(Header::read(&mut Cursor::new(&bad)).is_err());
    }
}

#[test]
fn tga_header_checks_raw_and_packet_extents_without_decoding() {
    let raw = fixture(3, 2, 2, 32, 0, 0x20);
    let header = Header::read(&mut Cursor::new(&raw)).unwrap();
    assert!(header.check_extent(raw.len() as u64).is_ok());
    assert!(header.check_extent(raw.len() as u64 - 1).is_err());

    let encoded = fixture(129, 1, 9, 16, 32, 0x20);
    let header = Header::read(&mut Cursor::new(&encoded)).unwrap();
    let minimum = 18 + 3 + 6 * 4 + 2 * (1 + 2);
    assert!(header.check_extent(minimum).is_ok());
    assert!(header.check_extent(minimum - 1).is_err());
}

#[test]
fn native_tga_orients_original_samples_in_one_fragment_and_retires_it_on_cancellation() {
    use crate::engine::native::worker::tests::Fixture;
    let root = Fixture::new();
    let (runtime, connection) = root.runtime_with_budgets(64 * 1024 * 1024, 64 * 1024 * 1024);
    let session = runtime.begin().unwrap();
    let base =
        NativeDocument::new_blank(4, 4, 16, &runtime.resident(), &runtime.disk(), &|| Ok(()))
            .unwrap();
    let path = root.path().join("source.TGA");
    let bytes = fixture(259, 259, 9, 16, 32, 0x10);
    std::fs::write(&path, &bytes).unwrap();
    let expected = image::load_from_memory_with_format(&bytes, image::ImageFormat::Tga)
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
                Err("TGA interrupted".into())
            } else {
                Ok(())
            }
        })
        .err()
        .unwrap();
        assert!(reason.contains("TGA interrupted"), "{reason}");
        assert_eq!(runtime.disk().used().unwrap(), baseline);
    }
    std::fs::write(&path, &bytes[..bytes.len() - 1]).unwrap();
    assert!(prepare(&base, &session, &path, &|| Ok(())).is_err());
    assert_eq!(runtime.disk().used().unwrap(), baseline);
    drop((base, session, runtime, connection));
}

#[test]
fn sparse_rle_tga_preserves_hidden_color_across_empty_bottom_up_stripes() {
    use crate::engine::native::worker::tests::Fixture;
    use crate::raster::backed::Region;

    let root = Fixture::new();
    let (runtime, connection) = root.runtime_with_budgets(64 * 1024 * 1024, 64 * 1024 * 1024);
    let session = runtime.begin().unwrap();
    let base = NativeDocument::new_blank(3, 259, 8, &runtime.resident(), &runtime.disk(), &|| Ok(()))
        .unwrap();
    let path = root.path().join("sparse.tga");
    let mut bytes = vec![0; 18];
    bytes[2] = 10;
    bytes[12..14].copy_from_slice(&3u16.to_le_bytes());
    bytes[14..16].copy_from_slice(&259u16.to_le_bytes());
    bytes[16..18].copy_from_slice(&[32, 0x18]);
    bytes.extend_from_slice(&[0, 11, 22, 33, 193]);
    let mut remaining = 3 * 259 - 2;
    while remaining > 0 {
        let count = remaining.min(128);
        bytes.extend_from_slice(&[0x80 | (count - 1) as u8, 0, 0, 0, 0]);
        remaining -= count;
    }
    bytes.extend_from_slice(&[0, 9, 17, 41, 0]);
    std::fs::write(&path, &bytes).unwrap();
    let output = prepare(&base, &session, &path, &|| Ok(())).unwrap();
    let pixels = output.surface()
        .prepare_region(Region::new([0, 0, 3, 259]).unwrap(), &mut || Ok(()))
        .unwrap();
    assert_eq!(pixels.pixel(0, 0).unwrap(), [41, 17, 9, 0]);
    assert_eq!(pixels.pixel(1, 128).unwrap(), [0; 4]);
    assert_eq!(pixels.pixel(2, 258).unwrap(), [33, 22, 11, 193]);
    drop(pixels);
    output.abort(&session, &|| session.cleanup_check()).unwrap();
    assert_eq!(std::fs::read(path).unwrap(), bytes);
    drop((base, session, runtime, connection));
}
