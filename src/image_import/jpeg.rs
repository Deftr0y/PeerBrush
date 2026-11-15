//! Bounded JPEG structure and original ICC admission, independent of decoder metadata fallbacks.
use crate::raster::CanvasLimits;
use std::io::{BufRead, Cursor, Read, Seek, SeekFrom};

const METADATA_LIMIT: usize = 8 * 1024 * 1024;
mod native;
pub(crate) use native::prepare;
pub(super) use native::{prepare_source, FILE_LIMITS};

pub(crate) fn matches(path: &std::path::Path) -> bool {
    path.extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("jpg") || e.eq_ignore_ascii_case("jpeg"))
}

pub(crate) fn check_reader(input: &mut (impl BufRead + Seek)) -> Result<(), String> {
    let length = input.seek(SeekFrom::End(0)).map_err(|e| e.to_string())?;
    Inspection::read(input, length, CanvasLimits::Resident, false, &|| Ok(()))?;
    input.seek(SeekFrom::Start(0)).map_err(|e| e.to_string())?;
    Ok(())
}

#[derive(Clone, Copy, Default)]
struct Part {
    offset: u64,
    length: usize,
}

pub(super) struct Inspection {
    pub width: u32,
    pub height: u32,
    pub components: u8,
    pub progressive: bool,
    sampling: [[u8; 2]; 4],
    first_scan_components: u8,
    parts: [Part; 255],
    count: usize,
    profile_bytes: usize,
}

impl Inspection {
    pub(super) fn read(
        input: &mut (impl BufRead + Seek),
        length: u64,
        limits: CanvasLimits,
        managed: bool,
        check: &dyn Fn() -> Result<(), String>,
    ) -> Result<Self, String> {
        check()?;
        match limits {
            CanvasLimits::Resident if length > super::ENCODED_LIMIT as u64 => {
                return Err("Encoded JPEG exceeds 128 MiB".into());
            }
            CanvasLimits::Native if length > FILE_LIMITS.encoded_bytes => {
                return Err("Encoded JPEG exceeds 3 GiB".into());
            }
            _ => {}
        }
        input.seek(SeekFrom::Start(0)).map_err(|e| e.to_string())?;
        if read::<2>(input)? != [0xff, 0xd8] {
            return Err("Invalid JPEG signature".into());
        }
        let mut result = Self {
            width: 0,
            height: 0,
            components: 0,
            progressive: false,
            sampling: [[0; 2]; 4],
            first_scan_components: 0,
            parts: [Part::default(); 255],
            count: 0,
            profile_bytes: 0,
        };
        let mut entropy = false;
        let mut scans = 0usize;
        let mut metadata = 0usize;
        let mut segments = 0usize;
        let mut found = 0usize;
        loop {
            check()?;
            let position = input.stream_position().map_err(|e| e.to_string())?;
            let marker = marker(&mut (&mut *input).take(length.saturating_sub(position)), entropy, check)?;
            entropy = false;
            segments += 1;
            if segments > 65536 {
                return Err("JPEG marker count exceeds admission".into());
            }
            if marker == 0xd9 {
                if result.components == 0 || scans == 0 {
                    return Err("JPEG ended without image data".into());
                }
                if found != result.count
                    || result.parts[..result.count].iter().any(|p| p.length == 0)
                {
                    return Err("JPEG ICC profile has missing fragments".into());
                }
                return Ok(result);
            }
            if matches!(marker, 0xd8 | 0xd0..=0xd7 | 0x01) {
                return Err("Unexpected standalone JPEG marker".into());
            }
            let size = usize::from(u16::from_be_bytes(read::<2>(input)?))
                .checked_sub(2)
                .ok_or("Invalid JPEG segment length")?;
            let start = input.stream_position().map_err(|e| e.to_string())?;
            let end = start
                .checked_add(size as u64)
                .filter(|end| *end <= length)
                .ok_or("Truncated JPEG segment")?;
            let mut prefix = [0u8; 32];
            let used = size.min(prefix.len());
            input
                .read_exact(&mut prefix[..used])
                .map_err(|_| "Truncated JPEG segment")?;
            let data = &prefix[..used];
            match marker {
                0xc0 | 0xc1 | 0xc2 => {
                    if result.components != 0 || size < 6 {
                        return Err("Invalid or repeated JPEG frame header".into());
                    }
                    if data[0] != 8 {
                        return Err("JPEG channel precision is not supported; convert explicitly before importing".into());
                    }
                    let channels = data[5];
                    if !matches!(channels, 1 | 3 | 4) || size != 6 + usize::from(channels) * 3 {
                        return Err("Unsupported JPEG component layout".into());
                    }
                    if managed && channels == 4 {
                        return Err("CMYK/four-channel JPEG requires explicit RGB conversion before importing".into());
                    }
                    result.width = u32::from(u16::from_be_bytes([data[3], data[4]]));
                    result.height = u32::from(u16::from_be_bytes([data[1], data[2]]));
                    result.components = channels;
                    result.progressive = marker == 0xc2;
                    limits.check(result.width, result.height)?;
                    let mut samples = 0u32;
                    for (index, component) in data[6..].chunks_exact(3).enumerate() {
                        let h = component[1] >> 4;
                        let v = component[1] & 15;
                        if !(1..=4).contains(&h)
                            || !(1..=4).contains(&v)
                            || component[2] > 3
                            || data[6..6 + index * 3]
                                .chunks_exact(3)
                                .any(|old| old[0] == component[0])
                        {
                            return Err("Invalid JPEG component sampling or identifier".into());
                        }
                        samples += u32::from(h) * u32::from(v);
                        result.sampling[index] = [h, v];
                    }
                    if samples > 10 {
                        return Err("JPEG sampling exceeds MCU admission".into());
                    }
                }
                0xc0..=0xcf if !matches!(marker, 0xc4 | 0xcc) => {
                    return Err("Unsupported JPEG coding process".into())
                }
                0xda => {
                    if result.components == 0
                        || size < 6
                        || data[0] == 0
                        || data[0] > result.components
                        || size != 4 + usize::from(data[0]) * 2
                    {
                        return Err("Invalid JPEG scan header".into());
                    }
                    scans += 1;
                    if scans == 1 {
                        result.first_scan_components = data[0];
                    }
                    if scans > 100 {
                        return Err("JPEG scan count exceeds admission".into());
                    }
                    entropy = true;
                }
                0xe0..=0xef | 0xfe => {
                    metadata = metadata
                        .checked_add(size)
                        .filter(|n| *n <= METADATA_LIMIT)
                        .ok_or("JPEG metadata exceeds 8 MiB")?;
                    if managed && marker == 0xe2 && data.starts_with(b"ICC_PROFILE\0") {
                        if size <= 14 || scans != 0 {
                            return Err(
                                "JPEG ICC fragment is empty or appears after image data".into()
                            );
                        }
                        let sequence = usize::from(data[12]);
                        let count = usize::from(data[13]);
                        if count == 0
                            || sequence == 0
                            || sequence > count
                            || (result.count != 0 && result.count != count)
                        {
                            return Err("JPEG ICC fragment count or sequence is invalid".into());
                        }
                        let part = &mut result.parts[sequence - 1];
                        if part.length != 0 {
                            return Err("JPEG ICC profile has duplicate fragments".into());
                        }
                        result.count = count;
                        found += 1;
                        *part = Part {
                            offset: start + 14,
                            length: size - 14,
                        };
                        result.profile_bytes = result
                            .profile_bytes
                            .checked_add(part.length)
                            .filter(|n| *n <= crate::color_profile::MAX_PROFILE_BYTES)
                            .ok_or("JPEG ICC profile exceeds 4 MiB")?;
                    }
                    if managed && scans != 0 && marker == 0xe1 && data.starts_with(b"Exif\0\0") {
                        return Err("JPEG orientation metadata appears after image data".into());
                    }
                }
                _ => {}
            }
            input
                .seek(SeekFrom::Start(end))
                .map_err(|e| e.to_string())?;
        }
    }

    fn profile(
        &self,
        bytes: &[u8],
        check: &dyn Fn() -> Result<(), String>,
    ) -> Result<Option<Vec<u8>>, String> {
        if self.count == 0 {
            return Ok(None);
        }
        let mut profile = Vec::new();
        profile
            .try_reserve_exact(self.profile_bytes)
            .map_err(|_| "JPEG profile allocation failed")?;
        for part in &self.parts[..self.count] {
            check()?;
            let start = usize::try_from(part.offset)
                .map_err(|_| "JPEG ICC offset exceeds address space")?;
            let data = bytes
                .get(
                    start
                        ..start
                            .checked_add(part.length)
                            .ok_or("JPEG ICC range overflow")?,
                )
                .ok_or("Truncated JPEG ICC fragment")?;
            profile.extend_from_slice(data);
        }
        check()?;
        Ok(Some(profile))
    }
}

fn read<const N: usize>(input: &mut impl Read) -> Result<[u8; N], String> {
    let mut bytes = [0; N];
    input
        .read_exact(&mut bytes)
        .map_err(|_| "Truncated JPEG marker or entropy stream")?;
    Ok(bytes)
}

fn marker(
    input: &mut impl BufRead,
    entropy: bool,
    check: &dyn Fn() -> Result<(), String>,
) -> Result<u8, String> {
    let mut prefix = false;
    loop {
        check()?;
        let bytes = input.fill_buf().map_err(|e| e.to_string())?;
        let count = bytes.len().min(64 * 1024);
        if count == 0 {
            return Err("JPEG has no complete end marker".into());
        }
        for (index, &value) in bytes[..count].iter().enumerate() {
            if value == 0xff {
                prefix = true;
            } else if prefix {
                prefix = false;
                if entropy && matches!(value, 0 | 0xd0..=0xd7) {
                    continue;
                }
                if value == 0 {
                    return Err("Stuffed JPEG byte outside entropy data".into());
                }
                input.consume(index + 1);
                return Ok(value);
            } else if !entropy {
                return Err("Invalid JPEG marker prefix".into());
            }
        }
        input.consume(count);
    }
}

pub(super) fn inspect(
    bytes: &[u8],
    check: &dyn Fn() -> Result<(), String>,
) -> Result<Inspection, String> {
    Inspection::read(&mut Cursor::new(bytes), bytes.len() as u64, CanvasLimits::Resident, true, check)
}

pub(super) fn inspect_file(path: &std::path::Path) -> Result<super::Info, String> {
    if !path.is_absolute() {
        return Err("Use an absolute local image path".into());
    }
    let file = crate::file_safety::open_native_read(path).map_err(|e| format!("Cannot read image: {e}"))?;
    let length = file.metadata().map_err(|e| e.to_string())?.len();
    let mut reader = std::io::BufReader::with_capacity(64 * 1024, file);
    let info = Inspection::read(&mut reader, length, CanvasLimits::Native, true, &|| Ok(()))?;
    Ok(super::Info {
        format: "Jpeg".into(), width: info.width, height: info.height, bit_depth: 8,
        choice: None, count: 1,
    })
}

pub(super) fn profile(
    bytes: &[u8],
    check: &dyn Fn() -> Result<(), String>,
) -> Result<Option<Vec<u8>>, String> {
    inspect(bytes, check)?.profile(bytes, check)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn segment(bytes: &mut Vec<u8>, marker: u8, data: &[u8]) {
        bytes.extend_from_slice(&[0xff, marker]);
        bytes.extend_from_slice(&((data.len() + 2) as u16).to_be_bytes());
        bytes.extend_from_slice(data);
    }
    fn fixture(parts: &[(u8, u8, &[u8])]) -> Vec<u8> {
        let mut bytes = vec![0xff, 0xd8];
        for &(sequence, count, data) in parts {
            let mut part = b"ICC_PROFILE\0".to_vec();
            part.extend_from_slice(&[sequence, count]);
            part.extend_from_slice(data);
            segment(&mut bytes, 0xe2, &part);
        }
        segment(
            &mut bytes,
            0xc2,
            &[8, 0, 1, 0, 2, 3, 1, 0x11, 0, 2, 0x11, 0, 3, 0x11, 0],
        );
        segment(&mut bytes, 0xda, &[3, 1, 0, 2, 0, 3, 0, 0, 63, 0]);
        bytes.extend_from_slice(&[19, 0xff, 0, 23, 0xff, 0xd0, 29, 0xff, 0xff, 0xd9]);
        bytes
    }

    #[test]
    fn complete_fragment_sequences_are_ordered_without_decoder_fallback() {
        let bytes = fixture(&[(2, 2, b"second"), (1, 2, b"first")]);
        let info = inspect(&bytes, &|| Ok(())).unwrap();
        assert_eq!(
            (info.width, info.height, info.components, info.progressive),
            (2, 1, 3, true)
        );
        assert_eq!(
            info.profile(&bytes, &|| Ok(())).unwrap().unwrap(),
            b"firstsecond"
        );
        let parts: Vec<_> = (1..=255).rev().map(|n| (n, 255, b"x".as_slice())).collect();
        let bytes = fixture(&parts);
        assert_eq!(
            profile(&bytes, &|| Ok(())).unwrap().unwrap(),
            vec![b'x'; 255]
        );
        assert!(profile(&fixture(&[]), &|| Ok(())).unwrap().is_none());
    }

    #[test]
    fn inspection_cannot_finish_a_marker_past_the_admitted_file_length() {
        let bytes = fixture(&[]);
        let length = bytes.len() as u64;
        assert!(Inspection::read(&mut Cursor::new(&bytes), length, CanvasLimits::Native, true, &|| Ok(())).is_ok());
        assert!(Inspection::read(&mut Cursor::new(&bytes), length - 1, CanvasLimits::Native, true, &|| Ok(())).is_err());
    }

    #[test]
    fn native_jpeg_inspection_and_open_share_the_encoded_limit() {
        let bytes = fixture(&[]);
        let limit = super::super::native::OpenFormat::Jpeg.encoded_limit();
        assert_eq!(limit, 3 * 1024 * 1024 * 1024);
        let inspect = |length, limits| {
            Inspection::read(&mut Cursor::new(&bytes), length, limits, true, &|| Ok(()))
        };
        assert!(inspect(limit, CanvasLimits::Native).is_ok());
        assert_eq!(inspect(limit + 1, CanvasLimits::Native).err().unwrap(), "Encoded JPEG exceeds 3 GiB");
        let resident_over = super::super::ENCODED_LIMIT as u64 + 1;
        assert!(inspect(resident_over, CanvasLimits::Native).is_ok());
        assert_eq!(inspect(resident_over, CanvasLimits::Resident).err().unwrap(), "Encoded JPEG exceeds 128 MiB");
    }

    #[test]
    fn entropy_scans_cross_refills_and_check_cancellation_per_chunk() {
        use std::{cell::Cell, io::BufReader};

        let bytes = [7, 0xff, 0, 0xff, 0xd0, 0xff, 0xff, 0xd7, 5, 0xff, 0xff, 0xd9, 42];
        for capacity in 1..=bytes.len() {
            let mut input = BufReader::with_capacity(capacity, Cursor::new(bytes));
            assert_eq!(marker(&mut input, true, &|| Ok(())).unwrap(), 0xd9);
            assert_eq!(read::<1>(&mut input).unwrap(), [42]);
        }

        let mut dense = [0xff, 0].repeat(32 * 1024);
        dense.extend_from_slice(&[0xff, 0xd9]);
        let checks = Cell::new(0);
        let mut input = Cursor::new(dense);
        let check = || {
            checks.set(checks.get() + 1);
            if checks.get() == 1 { Ok(()) } else { Err("JPEG scan cancelled".into()) }
        };
        assert_eq!(marker(&mut input, true, &check).unwrap_err(), "JPEG scan cancelled");
        assert_eq!(input.position(), 64 * 1024);
    }

    #[test]
    fn malformed_profile_structure_and_all_truncated_prefixes_are_rejected() {
        for parts in [
            vec![(0, 1, b"x".as_slice())],
            vec![(2, 1, b"x".as_slice())],
            vec![(1, 0, b"x".as_slice())],
            vec![(1, 2, b"x".as_slice())],
            vec![(1, 1, b"x".as_slice()), (1, 1, b"y".as_slice())],
            vec![(1, 2, b"x".as_slice()), (2, 3, b"y".as_slice())],
            vec![(1, 1, b"".as_slice())],
        ] {
            assert!(inspect(&fixture(&parts), &|| Ok(())).is_err());
        }
        let bytes = fixture(&[(1, 1, b"profile")]);
        for end in 0..bytes.len() {
            assert!(inspect(&bytes[..end], &|| Ok(())).is_err(), "prefix {end}");
        }
        let mut late = fixture(&[]);
        late.truncate(late.len() - 4);
        let mut part = b"ICC_PROFILE\0\x01\x01".to_vec();
        part.extend_from_slice(b"profile");
        segment(&mut late, 0xe2, &part);
        late.extend_from_slice(&[0xff, 0xd9]);
        assert!(inspect(&late, &|| Ok(())).err().unwrap().contains("after"));
    }

    #[test]
    fn original_metadata_and_profile_limits_apply_before_decoder_allocation() {
        let payload = vec![7u8; 65519];
        let parts: Vec<_> = (1..=65).map(|n| (n, 65, payload.as_slice())).collect();
        let reason = inspect(&fixture(&parts), &|| Ok(())).err().unwrap();
        assert!(reason.contains("4 MiB"), "{reason}");
        let mut bytes = vec![0xff, 0xd8];
        for _ in 0..129 {
            segment(&mut bytes, 0xfe, &vec![0; 65533]);
        }
        bytes.extend_from_slice(&fixture(&[])[2..]);
        assert!(inspect(&bytes, &|| Ok(())).err().unwrap().contains("8 MiB"));
        let calls = std::cell::Cell::new(0);
        let reason = inspect(&fixture(&[]), &|| {
            calls.set(calls.get() + 1);
            if calls.get() == 5 {
                Err("JPEG cancelled".into())
            } else {
                Ok(())
            }
        })
        .err()
        .unwrap();
        assert_eq!(reason, "JPEG cancelled");
    }
}
