//! Refuse channel truncation and color encodings the installed BMP codec ignores.
use std::io::{Read, Seek, SeekFrom};

pub(crate) fn validate_dib(bytes: &[u8]) -> Result<(), String> {
    let at = |offset, length| super::super::bytes_at(bytes, offset, length);
    let size = u32::from_le_bytes(at(0, 4)?.try_into().unwrap()) as usize;
    if !matches!(size, 12 | 40 | 52 | 56 | 108 | 124) {
        return Err("Unsupported BMP DIB header".into());
    }
    at(0, size)?;
    if size != 12 && u32::from_le_bytes(at(16, 4)?.try_into().unwrap()) == 3 {
        for mask in at(40, if size >= 56 { 16 } else { 12 })?.chunks_exact(4) {
            if u32::from_le_bytes(mask.try_into().unwrap()).count_ones() > 8 {
                return Err("BMP channels wider than 8 bits require explicit precision-preserving conversion before importing".into());
            }
        }
    }
    if size >= 108 {
        let space = u32::from_le_bytes(at(56, 4)?.try_into().unwrap());
        if !matches!(space, 0x73524742 | 0x57696e20)
            && (space != 0 || at(60, 48)?.iter().any(|&v| v != 0))
        {
            return Err("Calibrated/profiled BMP color encoding requires explicit RGB conversion before importing".into());
        }
    }
    Ok(())
}

fn read_dib(input: &mut impl Read) -> Result<(), String> {
    let mut bytes = [0; 124];
    input
        .read_exact(&mut bytes[..4])
        .map_err(|e| e.to_string())?;
    let size = u32::from_le_bytes(bytes[..4].try_into().unwrap()) as usize;
    if !matches!(size, 12 | 40 | 52 | 56 | 108 | 124) {
        return Err("Unsupported BMP DIB header".into());
    }
    input
        .read_exact(&mut bytes[4..size])
        .map_err(|e| e.to_string())?;
    let end = if size == 40 && u32::from_le_bytes(bytes[16..20].try_into().unwrap()) == 3 {
        input
            .read_exact(&mut bytes[40..52])
            .map_err(|e| e.to_string())?;
        52
    } else {
        size
    };
    validate_dib(&bytes[..end])
}

/// Inspect the same held reader before the generic patch decoder consumes it.
pub(crate) fn check_reader(
    input: &mut (impl Read + Seek),
    format: image::ImageFormat,
) -> Result<(), String> {
    let start = input.stream_position().map_err(|e| e.to_string())?;
    match format {
        image::ImageFormat::Bmp => {
            let mut file = [0; 14];
            input.read_exact(&mut file).map_err(|e| e.to_string())?;
            if &file[..2] != b"BM" {
                return Err("Invalid BMP signature".into());
            }
            read_dib(input)?;
        }
        image::ImageFormat::Ico => {
            let entry = super::super::ico::Entry::read(input, None)?;
            let length = entry.length;
            let offset = entry.offset;
            let position = start.checked_add(offset).ok_or("Icon offset overflow")?;
            input
                .seek(SeekFrom::Start(position))
                .map_err(|e| e.to_string())?;
            let mut signature = [0; 8];
            input
                .read_exact(&mut signature)
                .map_err(|e| e.to_string())?;
            if signature != *b"\x89PNG\r\n\x1a\n" {
                input
                    .seek(SeekFrom::Start(position))
                    .map_err(|e| e.to_string())?;
                read_dib(&mut input.by_ref().take(length))?;
            }
        }
        _ => return Ok(()),
    }
    input
        .seek(SeekFrom::Start(start))
        .map_err(|e| e.to_string())?;
    Ok(())
}
