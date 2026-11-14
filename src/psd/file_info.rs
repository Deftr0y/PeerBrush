//! Bounded Photoshop File Info and presentation metadata, kept as standard resources.
use super::{row_stream::Source, structural, Cursor};
use std::io::{Read, Seek};

pub(crate) const MAX_BYTES: usize = 1024 * 1024;
pub(crate) const SOURCE_MISMATCH: &str =
    "Source36 File Info differs from standard Photoshop data";
const LEGACY_MAC_PRINT_INFO: u16 = 1001;
const CAPTION: u16 = 1008;
const BORDER: u16 = 1009;
const BACKGROUND_COLOR: u16 = 1010;
const GRAYSCALE_HALFTONE: u16 = 1012;
const COLOR_HALFTONE: u16 = 1013;
const GRAYSCALE_TRANSFER: u16 = 1015;
const COLOR_TRANSFER: u16 = 1016;
const DOT_RANGE: u16 = 1019;
const JPEG_QUALITY: u16 = 1030;
const FILE_INFO_URL: u16 = 1035;
const LEGACY_COLOR_SAMPLERS: u16 = 1038;
const WATERMARK: u16 = 1040;
const WORKFLOW_URL: u16 = 1051;
const URL_LIST: u16 = 1054;
const LEGACY_PRINT_INFO: u16 = 1071;
const COLOR_SAMPLERS: u16 = 1073;
const MEASUREMENT_SCALE: u16 = 1074;
const PRINT_INFORMATION: u16 = 1082;
const PRINT_STYLE: u16 = 1083;
const MAC_PRINT_INFO: u16 = 1084;
const WINDOWS_DEVMODE: u16 = 1085;
const MAX_DESCRIPTOR_BYTES: usize = 16 * 1024;
pub(super) const IDS: [u16; 27] = [
    LEGACY_MAC_PRINT_INFO, CAPTION, BORDER, BACKGROUND_COLOR, GRAYSCALE_HALFTONE, COLOR_HALFTONE,
    GRAYSCALE_TRANSFER, COLOR_TRANSFER, DOT_RANGE, 1028, JPEG_QUALITY, FILE_INFO_URL,
    LEGACY_COLOR_SAMPLERS, WATERMARK, WORKFLOW_URL, URL_LIST, 1058, 1059, 1060, 1061, LEGACY_PRINT_INFO,
    COLOR_SAMPLERS, MEASUREMENT_SCALE, PRINT_INFORMATION, PRINT_STYLE, MAC_PRINT_INFO, WINDOWS_DEVMODE,
];

pub(super) fn supported(id: u16) -> bool {
    IDS.contains(&id)
}

fn record<'a>(reader: &mut Cursor<'a>) -> Result<(u16, &'a [u8]), String> {
    if reader.bytes(4)? != b"8BIM" {
        return Err("Invalid Photoshop File Info signature".into());
    }
    let id = reader.u16()?;
    let name = reader.u8()? as usize;
    reader.bytes(name + (name + 1) % 2)?;
    let payload = reader.block()?;
    reader.bytes(payload.len() % 2)?;
    Ok((id, payload))
}

pub(super) fn source_format(bytes: &[u8]) -> u32 {
    let mut reader = Cursor::new(bytes);
    let mut format = super::FILE_INFO_FORMAT;
    while reader.pos < bytes.len() {
        let Ok((id, _)) = record(&mut reader) else { return super::DOT_RANGE_FORMAT };
        let minimum = match id {
            GRAYSCALE_HALFTONE | COLOR_HALFTONE | GRAYSCALE_TRANSFER | COLOR_TRANSFER =>
                super::LEGACY_PRINT_SETUP_FORMAT,
            DOT_RANGE => super::DOT_RANGE_FORMAT,
            JPEG_QUALITY => super::JPEG_QUALITY_RESOURCE_FORMAT,
            WATERMARK | URL_LIST => super::WATERMARK_URL_LIST_FORMAT,
            LEGACY_MAC_PRINT_INFO | LEGACY_PRINT_INFO | LEGACY_COLOR_SAMPLERS | COLOR_SAMPLERS =>
                super::LEGACY_PRINT_SAMPLER_FORMAT,
            MEASUREMENT_SCALE => super::MEASUREMENT_SCALE_FORMAT,
            MAC_PRINT_INFO | WINDOWS_DEVMODE => super::PLATFORM_PRINT_FORMAT,
            PRINT_INFORMATION | PRINT_STYLE => super::PRINT_DESCRIPTOR_FORMAT,
            CAPTION | BORDER | BACKGROUND_COLOR | FILE_INFO_URL | WORKFLOW_URL => super::EXTENDED_FILE_INFO_FORMAT,
            _ => super::FILE_INFO_FORMAT,
        };
        format = format.max(minimum);
    }
    format
}

fn valid_payload(id: u16, payload: &[u8]) -> bool {
    match id {
        CAPTION => payload.split_first().is_some_and(|(&len, text)| text.len() == usize::from(len)),
        BORDER => matches!(payload, [_, _, _, _, 0, 1..=5]),
        // Color spaces include vendor-specific inks. Preserve the complete
        // standard color record without converting or interpreting its words.
        BACKGROUND_COLOR => payload.len() == 10,
        GRAYSCALE_HALFTONE | COLOR_HALFTONE => {
            let (screens, remainder) = payload.as_chunks::<18>();
            // Standard dot shapes have nonnegative codes. Negative codes can
            // carry custom PostScript spot functions and remain protected.
            remainder.is_empty() && (1..=4).contains(&screens.len()) && screens.iter().all(|screen| {
                let frequency = i32::from_be_bytes(screen[..4].try_into().unwrap());
                let angle = i32::from_be_bytes(screen[6..10].try_into().unwrap());
                (65536..1000 * 65536).contains(&frequency)
                    && (-180 * 65536..=180 * 65536).contains(&angle)
                    && matches!(&screen[4..6], [0, 1 | 2])
                    && matches!(&screen[10..12], [0, 0..=4 | 6])
                    && screen[12..16] == [0; 4]
                    && screen[16..18].iter().all(|&flag| flag <= 1)
            })
        }
        GRAYSCALE_TRANSFER | COLOR_TRANSFER => {
            let (curves, remainder) = payload.as_chunks::<28>();
            remainder.is_empty() && (1..=4).contains(&curves.len()) && curves.iter().all(|curve| {
                let (points, _) = curve[..26].as_chunks::<2>();
                points.iter().enumerate().all(|(index, &point)| {
                    let point = i16::from_be_bytes(point);
                    (0..=1000).contains(&point) || (index != 0 && index != 12 && point == -1)
                }) && matches!(&curve[26..], [0, 0 | 1])
            })
        }
        DOT_RANGE => payload.len() == 2,
        // Preserve the optional seventh byte without interpreting it.
        JPEG_QUALITY => match payload {
            [hi, lo, format_hi, format_lo, scan_hi, scan_lo, ..] if payload.len() <= 7 => {
                let quality = i16::from_be_bytes([*hi, *lo]);
                let format = u16::from_be_bytes([*format_hi, *format_lo]);
                let scans = u16::from_be_bytes([*scan_hi, *scan_lo]);
                (-4..=8).contains(&quality) && match format {
                    0 | 1 => true,
                    0x0101 => (1..=3).contains(&scans),
                    _ => false,
                }
            }
            _ => false,
        },
        // Non-empty sampler records contain coordinates that canvas edits must update.
        LEGACY_COLOR_SAMPLERS | COLOR_SAMPLERS => matches!(payload, [0, 0, 0, 1..=3, 0, 0, 0, 0]),
        WATERMARK => payload.len() == 1,
        // Entries carry IDs whose associations cannot be maintained by the editor.
        URL_LIST => payload == [0; 4],
        WORKFLOW_URL => {
            let Some((length, text)) = payload.split_at_checked(4) else { return false };
            let length = u32::from_be_bytes(length.try_into().unwrap()) as usize;
            let (units, remainder) = text.as_chunks::<2>();
            if !remainder.is_empty() {
                return false;
            }
            let units = if units.len() == length {
                units.strip_suffix(&[[0; 2]]).unwrap_or(units)
            } else if units.len().checked_sub(1) == Some(length) && units.last() == Some(&[0; 2]) {
                &units[..length]
            } else {
                return false;
            };
            char::decode_utf16(units.iter().copied().map(u16::from_be_bytes))
                .all(|character| character.is_ok_and(|ch| ch != '\0'))
        }
        1061 => payload.len() == 16,
        MEASUREMENT_SCALE | PRINT_INFORMATION | PRINT_STYLE => payload.len() <= MAX_DESCRIPTOR_BYTES
            && super::styles::descriptor_block(payload).is_ok(),
        // Printer settings are opaque platform data. The resource collection
        // already bounds their size; the codec never loads a print driver.
        LEGACY_MAC_PRINT_INFO | LEGACY_PRINT_INFO | MAC_PRINT_INFO | WINDOWS_DEVMODE => !payload.is_empty(),
        _ => !payload.is_empty(),
    }
}

fn is_bounded_descriptor(id: u16, bytes: u64) -> bool {
    matches!(id, MEASUREMENT_SCALE | PRINT_INFORMATION | PRINT_STYLE)
        && bytes <= MAX_DESCRIPTOR_BYTES as u64
}

pub(super) fn descriptor_working_bytes(index: &structural::Index) -> Result<u64, String> {
    // Validation parses one bounded descriptor at a time and immediately drops it.
    let largest = index.resources.iter()
        .filter(|r| is_bounded_descriptor(r.id, r.data.bytes))
        .map(|r| r.data.bytes)
        .max();
    largest.map_or(Ok(0), super::styles::native_working_bytes)
}

pub(crate) fn validation_working_bytes(bytes: &[u8]) -> Result<usize, String> {
    if bytes.len() > MAX_BYTES {
        return Err("Photoshop File Info resources exceed 1 MiB".into());
    }
    let mut reader = Cursor::new(bytes);
    let mut largest = 0;
    while reader.pos < bytes.len() {
        let (id, data) = record(&mut reader)?;
        if is_bounded_descriptor(id, data.len() as u64) {
            largest = largest.max(data.len());
        }
    }
    if largest == 0 { return Ok(0); }
    Ok(super::styles::native_working_bytes(largest as u64)? as usize)
}

pub(crate) fn validate(bytes: &[u8]) -> Result<(), String> {
    if bytes.is_empty() {
        return Err("Empty Photoshop File Info resource collection".into());
    }
    if bytes.len() > MAX_BYTES {
        return Err("Photoshop File Info resources exceed 1 MiB".into());
    }
    let mut seen = [false; IDS.len()];
    let mut reader = Cursor::new(bytes);
    while reader.pos < bytes.len() {
        let (id, payload) = record(&mut reader)?;
        let slot = IDS.iter().position(|known| *known == id)
            .ok_or("Unexpected Photoshop File Info resource")?;
        if std::mem::replace(&mut seen[slot], true) {
            return Err("Duplicate Photoshop File Info resource".into());
        }
        if !valid_payload(id, payload) {
            if matches!(id, LEGACY_COLOR_SAMPLERS | COLOR_SAMPLERS) {
                return Err("Photoshop color sampler points or malformed sampler records remain protected".into());
            }
            if id == URL_LIST {
                return Err("Photoshop URL List entries or malformed URL List records remain protected".into());
            }
            return Err(format!("Invalid Photoshop File Info payload for resource {id}"));
        }
    }
    Ok(())
}

pub(super) fn record_span(resource: &structural::Resource) -> Result<structural::Span, String> {
    let offset = resource.name.offset.checked_sub(7)
        .ok_or("Invalid Photoshop File Info record offset")?;
    let end = resource.data.end()?.checked_add(resource.data.bytes % 2)
        .ok_or("Photoshop File Info record length overflow")?;
    let bytes = end.checked_sub(offset).ok_or("Invalid Photoshop File Info record range")?;
    Ok(structural::Span { offset, bytes })
}

pub(super) fn indexed_length(index: &structural::Index) -> Result<u64, String> {
    index.resources.iter().filter(|record| supported(record.id)).try_fold(0u64, |total, record| {
        total.checked_add(record_span(record)?.bytes).ok_or("Photoshop File Info length overflow".into())
    })
}

pub(super) fn inspect_indexed<R: Read + Seek>(
    source: &Source<R>,
    index: &structural::Index,
    warnings: &mut Vec<String>,
    check: &dyn Fn() -> Result<(), String>,
) -> Result<Option<Vec<u8>>, String> {
    let length = indexed_length(index)?;
    if length == 0 {
        return Ok(None);
    }
    if length > MAX_BYTES as u64 {
        warnings.push("Photoshop File Info resources exceed 1 MiB".into());
        return Ok(None);
    }
    let mut bytes = Vec::new();
    bytes.try_reserve_exact(length as usize)
        .map_err(|_| "Photoshop File Info allocation refused")?;
    let mut offset = 0;
    for resource in index.resources.iter().filter(|record| supported(record.id)) {
        let span = record_span(resource)?;
        let end = offset + span.bytes as usize;
        bytes.resize(end, 0);
        source.range(span, check)?.read_exact(&mut bytes[offset..end])
            .map_err(|e| e.to_string())?;
        offset = end;
    }
    check()?;
    match validate(&bytes) {
        Ok(()) => Ok(Some(bytes)),
        Err(reason) => {
            warnings.push(reason);
            Ok(None)
        }
    }
}

pub(super) fn matches_indexed<R: Read + Seek>(
    source: &Source<R>,
    index: &structural::Index,
    expected: &[u8],
    check: &dyn Fn() -> Result<(), String>,
) -> Result<bool, String> {
    if indexed_length(index)? != expected.len() as u64 {
        return Ok(false);
    }
    let mut offset = 0;
    let mut buffer = [0; 8192];
    for resource in index.resources.iter().filter(|record| supported(record.id)) {
        let span = record_span(resource)?;
        let end = offset + span.bytes as usize;
        let mut input = source.range(span, check)?;
        for part in expected[offset..end].chunks(buffer.len()) {
            check()?;
            input.read_exact(&mut buffer[..part.len()]).map_err(|e| e.to_string())?;
            if buffer[..part.len()] != *part {
                return Ok(false);
            }
        }
        offset = end;
    }
    check()?;
    Ok(true)
}

#[cfg(test)]
pub(crate) fn fixture() -> Vec<u8> {
    let mut bytes = Vec::new();
    let screen = [0, 60, 0, 0, 0, 1, 0, 45, 0, 0, 0, 6, 0, 0, 0, 0, 1, 0];
    let mut transfer = [0xff; 28];
    transfer[..2].copy_from_slice(&0i16.to_be_bytes());
    transfer[24..26].copy_from_slice(&1000i16.to_be_bytes());
    transfer[26..].copy_from_slice(&1u16.to_be_bytes());
    for (id, name, data) in [
        (LEGACY_MAC_PRINT_INFO, b"".as_slice(), &[0, 1, 0xff]),
        (CAPTION, b"caption".as_slice(), b"\x04Test".as_slice()),
        (BORDER, b"".as_slice(), &[0, 2, 128, 0, 0, 3]),
        (BACKGROUND_COLOR, b"".as_slice(), &[0, 0, 48, 59, 91, 163, 135, 11, 0, 0]),
        (GRAYSCALE_HALFTONE, b"".as_slice(), &screen),
        (COLOR_HALFTONE, b"".as_slice(), &screen),
        (GRAYSCALE_TRANSFER, b"".as_slice(), &transfer),
        (COLOR_TRANSFER, b"".as_slice(), &transfer),
        (DOT_RANGE, b"".as_slice(), &[12, 243]),
        (JPEG_QUALITY, b"".as_slice(), &[0, 6, 1, 1, 0, 2, 0]),
        (FILE_INFO_URL, b"".as_slice(), b"https://example.invalid/reference".as_slice()),
        (LEGACY_COLOR_SAMPLERS, b"".as_slice(), b"\0\0\0\x01\0\0\0\0".as_slice()),
        (WATERMARK, b"".as_slice(), &[1]),
        (WORKFLOW_URL, b"".as_slice(), b"\0\0\0\x02\0x\0\0".as_slice()),
        (URL_LIST, b"".as_slice(), &[0; 4]),
        (1060u16, b"meta".as_slice(), b"<x:xmpmeta xmlns:x='adobe:ns:meta/' />".as_slice()),
        (1058, b"".as_slice(), b"II\x2a\0\x08\0\0\0\0\0\0\0\0\0".as_slice()),
        (1061, b"".as_slice(), &[7; 16]),
        (LEGACY_PRINT_INFO, b"".as_slice(), &[0x10, 0x7f]),
        (COLOR_SAMPLERS, b"".as_slice(), b"\0\0\0\x03\0\0\0\0".as_slice()),
        (MEASUREMENT_SCALE, b"".as_slice(), b"\0\0\0\x10\0\0\0\0\0\0\0\0null\0\0\0\0".as_slice()),
        (PRINT_INFORMATION, b"print".as_slice(), b"\0\0\0\x10\0\0\0\0\0\0\0\0null\0\0\0\x01\0\0\0\0Cntnbool\x01".as_slice()),
        (PRINT_STYLE, b"".as_slice(), b"\0\0\0\x10\0\0\0\0\0\0\0\0null\0\0\0\0".as_slice()),
        (MAC_PRINT_INFO, b"mac".as_slice(), &[0x80, 0, 0xfe]),
        (WINDOWS_DEVMODE, b"win".as_slice(), &[0, 0xff, 0x10, 0]),
    ] {
        bytes.extend_from_slice(b"8BIM");
        bytes.extend_from_slice(&id.to_be_bytes());
        bytes.push(name.len() as u8);
        bytes.extend_from_slice(name);
        if (name.len() + 1) % 2 != 0 {
            bytes.push(0);
        }
        super::block(&mut bytes, data);
        if data.len() % 2 != 0 {
            bytes.push(0);
        }
    }
    bytes
}
