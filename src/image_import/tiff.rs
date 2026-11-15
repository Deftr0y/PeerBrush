//! Bounded TIFF directory inspection, independent of pixel decoder allocation.
use std::io::{Read, Seek, SeekFrom};
mod native;
pub(super) use native::{prepare_source, FILE_LIMITS};
pub(crate) use native::prepare;

pub(crate) fn matches(path: &std::path::Path) -> bool {
    path.extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("tif") || ext.eq_ignore_ascii_case("tiff"))
}

#[derive(Clone, Copy)]
struct Header {
    little: bool,
    big: bool,
}
impl Header {
    fn number(self, bytes: &[u8]) -> u64 {
        let mut value = 0u64;
        if self.little {
            for &byte in bytes.iter().rev() {
                value = (value << 8) | u64::from(byte);
            }
        } else {
            for &byte in bytes {
                value = (value << 8) | u64::from(byte);
            }
        }
        value
    }
    fn pointer(self) -> usize {
        if self.big {
            8
        } else {
            4
        }
    }
    fn count(self) -> usize {
        if self.big {
            8
        } else {
            2
        }
    }
    fn entry(self) -> usize {
        if self.big {
            20
        } else {
            12
        }
    }
    fn read_number(self, input: &mut impl Read, length: usize) -> Result<u64, String> {
        let mut bytes = [0; 8];
        input
            .read_exact(&mut bytes[..length])
            .map_err(|_| "Truncated TIFF directory")?;
        Ok(self.number(&bytes[..length]))
    }
}

pub(super) struct Directory {
    header: Header,
    offsets: [u64; super::MAX_ITEMS],
    count: usize,
    length: u64,
}
impl Directory {
    pub(super) fn read(
        input: &mut (impl Read + Seek),
        length: u64,
        check: &dyn Fn() -> Result<(), String>,
    ) -> Result<Self, String> {
        check()?;
        input.seek(SeekFrom::Start(0)).map_err(|e| e.to_string())?;
        let mut bytes = [0; 16];
        input
            .read_exact(&mut bytes[..8])
            .map_err(|_| "Truncated TIFF header")?;
        let little = match &bytes[..2] {
            b"II" => true,
            b"MM" => false,
            _ => return Err("Invalid TIFF byte order".into()),
        };
        let mut header = Header { little, big: false };
        header.big = match header.number(&bytes[2..4]) {
            42 => false,
            43 => true,
            _ => return Err("Invalid TIFF version".into()),
        };
        if header.big {
            if header.number(&bytes[4..6]) != 8 || header.number(&bytes[6..8]) != 0 {
                return Err("Invalid BigTIFF header".into());
            }
            input
                .read_exact(&mut bytes[8..])
                .map_err(|_| "Truncated BigTIFF header")?;
        }
        let mut at = header.number(if header.big {
            &bytes[8..16]
        } else {
            &bytes[4..8]
        });
        let mut directory = Self {
            header,
            offsets: [0; super::MAX_ITEMS],
            count: 0,
            length,
        };
        while at != 0 {
            check()?;
            if directory.count == super::MAX_ITEMS
                || directory.offsets[..directory.count].contains(&at)
            {
                return Err("TIFF page limit exceeded or cyclic page chain".into());
            }
            directory.offsets[directory.count] = at;
            directory.count += 1;
            let entries = directory.entries(input, at)?;
            let next = at + header.count() as u64 + entries * header.entry() as u64;
            input
                .seek(SeekFrom::Start(next))
                .map_err(|e| e.to_string())?;
            at = header.read_number(input, header.pointer())?;
        }
        if directory.count == 0 {
            return Err("TIFF contains no pages".into());
        }
        check()?;
        Ok(directory)
    }
    fn entries(&self, input: &mut (impl Read + Seek), at: u64) -> Result<u64, String> {
        if at
            .checked_add(self.header.count() as u64)
            .is_none_or(|end| end > self.length)
        {
            return Err("TIFF directory offset exceeds its source".into());
        }
        input.seek(SeekFrom::Start(at)).map_err(|e| e.to_string())?;
        let count = self.header.read_number(input, self.header.count())?;
        // TIFF tags are u16. A larger directory cannot describe unique tags.
        if count > 65536 {
            return Err("TIFF directory tag count exceeds admission".into());
        }
        count
            .checked_mul(self.header.entry() as u64)
            .and_then(|n| at.checked_add(n))
            .and_then(|n| n.checked_add((self.header.count() + self.header.pointer()) as u64))
            .filter(|&end| end <= self.length)
            .ok_or("Truncated TIFF directory entries")?;
        Ok(count)
    }
    pub(super) fn offsets(&self) -> &[u64] {
        &self.offsets[..self.count]
    }

    /// Bound the dimensions before the decoder constructs its strip/tile layout.
    pub(super) fn native_dimensions(
        &self,
        input: &mut (impl Read + Seek),
        page: usize,
        check: &dyn Fn() -> Result<(), String>,
    ) -> Result<(u32, u32), String> {
        self.native_header(input, page, false, check).map(|(size, _)| size)
    }

    fn native_header(
        &self,
        input: &mut (impl Read + Seek),
        page: usize,
        read_color: bool,
        check: &dyn Fn() -> Result<(), String>,
    ) -> Result<((u32, u32), Option<u16>), String> {
        let at = *self.offsets().get(page).ok_or("TIFF page index is outside the available items")?;
        let count = self.entries(input, at)?;
        let value_at = self.header.entry() - self.header.pointer();
        let mut bytes = [0; 20];
        let mut dimensions = [None; 2];
        let mut bits = None;
        let mut photometric = None;
        for _ in 0..count {
            check()?;
            input.read_exact(&mut bytes[..self.header.entry()]).map_err(|_| "Truncated TIFF entry")?;
            let tag = self.header.number(&bytes[..2]);
            let field = match tag {
                256 | 257 => &mut dimensions[(tag - 256) as usize],
                262 if read_color => &mut photometric,
                258 if read_color => {
                    if bits.replace(bytes).is_some() {
                        return Err("Ambiguous TIFF channel depth declaration".into());
                    }
                    continue;
                }
                _ => continue,
            };
            let size = match self.header.number(&bytes[2..4]) {
                1 => 1,
                3 => 2,
                4 | 13 => 4,
                16 | 18 if self.header.big => 8,
                _ => return Err("Invalid TIFF header field type".into()),
            };
            if field.is_some() || self.header.number(&bytes[4..value_at]) != 1 {
                return Err("Ambiguous TIFF image header field".into());
            }
            *field = Some(u32::try_from(self.header.number(&bytes[value_at..value_at + size]))
                .map_err(|_| "TIFF header field exceeds its supported range")?);
        }
        let [Some(width), Some(height)] = dimensions else {
            return Err("TIFF image dimensions are missing".into());
        };
        crate::raster::CanvasLimits::Native.check(width, height)?;
        if !read_color {
            return Ok(((width, height), None));
        }
        let depth = if let Some(entry) = bits {
            let samples = self.header.number(&entry[4..value_at]);
            if !(1..=4).contains(&samples) || self.header.number(&entry[2..4]) != 3 {
                return Err("Invalid TIFF channel depth declaration".into());
            }
            let size = samples as usize * 2;
            let mut values = [0; 8];
            if size <= self.header.pointer() {
                values[..size].copy_from_slice(&entry[value_at..value_at + size]);
            } else {
                let offset = self.header.number(&entry[value_at..self.header.entry()]);
                if offset.checked_add(size as u64).is_none_or(|end| end > self.length) {
                    return Err("TIFF channel depths exceed the source".into());
                }
                input.seek(SeekFrom::Start(offset)).map_err(|e| e.to_string())?;
                input.read_exact(&mut values[..size]).map_err(|_| "Truncated TIFF channel depths")?;
            }
            let first = self.header.number(&values[..2]);
            if !matches!(first, 1 | 8 | 16)
                || values[..size].chunks_exact(2).any(|v| self.header.number(v) != first)
            {
                return Err("TIFF channel precision requires explicit conversion".into());
            }
            first as u16
        } else {
            1
        };
        if !matches!((photometric, depth), (Some(0 | 1), _) | (Some(2), 8 | 16)) {
            return Err("TIFF color interpretation requires explicit RGB conversion".into());
        }
        Ok(((width, height), Some(depth.max(8))))
    }

    /// Inspect the selected page's original ICC declaration. The image wrapper
    /// converts TIFF tag errors to None, which cannot establish untagged color.
    pub(super) fn profile_range(
        &self,
        input: &mut (impl Read + Seek),
        page: usize,
        check: &dyn Fn() -> Result<(), String>,
    ) -> Result<Option<(u64, usize)>, String> {
        check()?;
        let at = *self
            .offsets()
            .get(page)
            .ok_or("TIFF page index is outside the available items")?;
        let count = self.entries(input, at)?;
        let mut result = None;
        let mut bytes = [0; 20];
        for _ in 0..count {
            check()?;
            let position = input.stream_position().map_err(|e| e.to_string())?;
            input
                .read_exact(&mut bytes[..self.header.entry()])
                .map_err(|_| "Truncated TIFF entry")?;
            if self.header.number(&bytes[..2]) != 34675 {
                continue;
            }
            if result.is_some() {
                return Err("TIFF page has ambiguous ICC profiles".into());
            }
            if !matches!(self.header.number(&bytes[2..4]), 1 | 7) {
                return Err("TIFF ICC profile must contain byte data".into());
            }
            let value_at = if self.header.big { 12 } else { 8 };
            let length = self.header.number(&bytes[4..value_at]);
            if length == 0 || length > crate::color_profile::MAX_PROFILE_BYTES as u64 {
                return Err("TIFF ICC profile exceeds its supported byte limit".into());
            }
            let offset = if length <= self.header.pointer() as u64 {
                position + value_at as u64
            } else {
                self.header.number(&bytes[value_at..self.header.entry()])
            };
            if offset
                .checked_add(length)
                .is_none_or(|end| end > self.length)
            {
                return Err("TIFF ICC profile exceeds its source bytes".into());
            }
            result = Some((offset, length as usize));
        }
        check()?;
        Ok(result)
    }
}

pub(super) fn inspect(path: &std::path::Path) -> Result<super::Info, String> {
    if !path.is_absolute() {
        return Err("Use an absolute local image path".into());
    }
    let file = crate::file_safety::open_native_read(path).map_err(|e| format!("Cannot read image: {e}"))?;
    let metadata = file.metadata().map_err(|e| e.to_string())?;
    if metadata.len() > FILE_LIMITS.encoded_bytes {
        return Err("Encoded TIFF image must be a file at most 3 GiB".into());
    }
    let mut input = std::io::BufReader::with_capacity(64 * 1024, file);
    let directory = Directory::read(&mut input, metadata.len(), &|| Ok(()))?;
    let ((width, height), bit_depth) = directory.native_header(&mut input, 0, true, &|| Ok(()))?;
    let count = directory.offsets().len();
    Ok(super::Info {
        format: "Tiff".into(), width, height, bit_depth: bit_depth.ok_or("TIFF channel precision missing")?,
        choice: (count > 1).then_some("page"), count,
    })
}

pub(super) fn profile<'a>(
    bytes: &'a [u8],
    page: usize,
    check: &dyn Fn() -> Result<(), String>,
) -> Result<Option<&'a [u8]>, String> {
    let mut input = std::io::Cursor::new(bytes);
    let directory = Directory::read(&mut input, bytes.len() as u64, check)?;
    directory
        .profile_range(&mut input, page, check)?
        .map(|(offset, length)| {
            let offset =
                usize::try_from(offset).map_err(|_| "TIFF ICC offset exceeds address space")?;
            super::bytes_at(bytes, offset, length)
        })
        .transpose()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{cell::Cell, io::Cursor};

    fn put(out: &mut [u8], number: u64, little: bool) {
        let count = out.len();
        if little {
            out.copy_from_slice(&number.to_le_bytes()[..count]);
        } else {
            out.copy_from_slice(&number.to_be_bytes()[8 - count..]);
        }
    }
    fn fixture(little: bool, big: bool, profiles: &[(u16, &[u8])]) -> Vec<u8> {
        let header = Header { little, big };
        let start = if big { 16 } else { 8 };
        let size = start + header.count() + profiles.len() * header.entry() + header.pointer();
        let mut bytes = vec![0; size];
        bytes[..2].copy_from_slice(if little { b"II" } else { b"MM" });
        put(&mut bytes[2..4], if big { 43 } else { 42 }, little);
        if big {
            put(&mut bytes[4..6], 8, little);
        }
        put(
            &mut bytes[if big { 8 } else { 4 }..start],
            start as u64,
            little,
        );
        put(
            &mut bytes[start..start + header.count()],
            profiles.len() as u64,
            little,
        );
        for (i, (kind, payload)) in profiles.iter().enumerate() {
            let offset = bytes.len();
            let at = start + header.count() + i * header.entry();
            let value_at = if big { 12 } else { 8 };
            let entry = &mut bytes[at..at + header.entry()];
            put(&mut entry[..2], 34675, little);
            put(&mut entry[2..4], u64::from(*kind), little);
            put(&mut entry[4..value_at], payload.len() as u64, little);
            if payload.len() <= header.pointer() {
                entry[value_at..value_at + payload.len()].copy_from_slice(payload);
            } else {
                put(&mut entry[value_at..], offset as u64, little);
                bytes.extend_from_slice(payload);
            }
        }
        bytes
    }
    #[test]
    fn classic_and_big_endian_directories_return_exact_original_profile_ranges() {
        for little in [false, true] {
            for big in [false, true] {
                for value in [
                    b"123".as_slice(),
                    b"exact original profile bytes".as_slice(),
                ] {
                    for kind in [1, 7] {
                        let bytes = fixture(little, big, &[(kind, value)]);
                        assert_eq!(profile(&bytes, 0, &|| Ok(())).unwrap(), Some(value));
                        for length in 0..bytes.len() {
                            assert!(
                                profile(&bytes[..length], 0, &|| Ok(())).is_err(),
                                "{little}/{big}/{length}"
                            );
                        }
                    }
                }
                assert_eq!(
                    profile(&fixture(little, big, &[]), 0, &|| Ok(())).unwrap(),
                    None
                );
                assert!(profile(
                    &fixture(little, big, &[(7, b"first"), (7, b"second")]),
                    0,
                    &|| Ok(())
                )
                .is_err());
                assert!(
                    profile(&fixture(little, big, &[(3, b"bad type")]), 0, &|| Ok(())).is_err()
                );
                assert!(profile(&fixture(little, big, &[(7, b"")]), 0, &|| Ok(())).is_err());
            }
        }
    }
    #[test]
    fn directory_cycles_huge_counts_offsets_and_cancellation_refuse() {
        let mut cyclic = fixture(true, false, &[]);
        cyclic[10..14].copy_from_slice(&8u32.to_le_bytes());
        assert!(profile(&cyclic, 0, &|| Ok(()))
            .err()
            .unwrap()
            .contains("cyclic"));
        let mut huge = fixture(true, true, &[]);
        huge[16..24].copy_from_slice(&65537u64.to_le_bytes());
        assert!(profile(&huge, 0, &|| Ok(()))
            .err()
            .unwrap()
            .contains("tag count"));
        let mut outside = fixture(true, false, &[(7, b"long profile")]);
        outside[18..22].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(profile(&outside, 0, &|| Ok(())).is_err());
        let bytes = fixture(true, true, &[(7, b"long profile")]);
        let calls = Cell::new(0);
        let result = profile(&bytes, 0, &|| {
            calls.set(calls.get() + 1);
            if calls.get() == 5 {
                Err("TIFF inspection interrupted".into())
            } else {
                Ok(())
            }
        });
        assert_eq!(result.err().unwrap(), "TIFF inspection interrupted");
        let mut input = Cursor::new(&bytes);
        let directory = Directory::read(&mut input, bytes.len() as u64, &|| Ok(())).unwrap();
        assert!(directory.profile_range(&mut input, 1, &|| Ok(())).is_err());
    }
}
