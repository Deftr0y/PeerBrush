//! Original WebP container admission shared by resident and native imports.
use crate::raster::CanvasLimits;
use std::io::{self, Read, Seek, SeekFrom};
use std::ops::Range;

mod native;
pub(super) use native::{prepare_source, FILE_LIMITS};
pub(crate) use native::prepare;

pub(crate) const NATIVE_FILE_BYTES: u64 = 3 * 1024 * 1024 * 1024;
const NATIVE_ALPHA_FILE_BYTES: u64 = 512 * 1024 * 1024;

#[derive(Debug)]
pub(crate) struct Info {
    pub width: u32,
    pub height: u32,
    pub frames: usize,
    pub animated: bool,
    pub lossy: bool,
    pub lossless: bool,
    pub alpha_chunk: bool,
    pub static_alpha: Option<StaticAlpha>,
    pub profile: Option<Range<u64>>,
    pub exif: Option<Range<u64>>,
}

#[derive(Debug)]
pub(crate) struct StaticAlpha {
    pub range: Range<u64>,
    pub header: u8,
}

impl StaticAlpha {
    pub fn compressed(&self) -> bool {
        self.header & 3 == 1
    }
}

impl Info {
    pub(super) fn native_file_limit(&self) -> u64 {
        if self.static_alpha.is_some() {
            NATIVE_ALPHA_FILE_BYTES
        } else if self.animated || (self.lossless && !self.lossy) {
            NATIVE_FILE_BYTES
        } else {
            super::ENCODED_LIMIT as u64
        }
    }
}

fn read<const N: usize>(input: &mut (impl Read + Seek), at: u64) -> Result<[u8; N], String> {
    input.seek(SeekFrom::Start(at)).map_err(|e| e.to_string())?;
    let mut bytes = [0; N];
    input
        .read_exact(&mut bytes)
        .map_err(|e| format!("Truncated WebP input: {e}"))?;
    Ok(bytes)
}
fn u24(bytes: &[u8]) -> u32 {
    u32::from_le_bytes([bytes[0], bytes[1], bytes[2], 0])
}
fn chunk(
    input: &mut (impl Read + Seek),
    at: u64,
    end: u64,
) -> Result<([u8; 4], Range<u64>, u64), String> {
    if end.saturating_sub(at) < 8 {
        return Err("Truncated WebP chunk header".into());
    }
    let h = read::<8>(input, at)?;
    let size = u32::from_le_bytes(h[4..].try_into().unwrap()) as u64;
    let data = at + 8..at + 8 + size;
    let next = data.end + (size & 1);
    if next > end {
        return Err("WebP chunk exceeds its container".into());
    }
    Ok((h[..4].try_into().unwrap(), data, next))
}
fn dimensions(
    input: &mut (impl Read + Seek),
    kind: [u8; 4],
    data: &Range<u64>,
) -> Result<(u32, u32), String> {
    let size = data.end - data.start;
    let (w, h) = if &kind == b"VP8L" {
        if size < 5 {
            return Err("Truncated WebP lossless header".into());
        }
        let h = read::<5>(input, data.start)?;
        let bits = u32::from_le_bytes(h[1..].try_into().unwrap());
        if h[0] != 0x2f || bits >> 29 != 0 {
            return Err("Invalid WebP lossless header".into());
        }
        ((bits & 0x3fff) + 1, ((bits >> 14) & 0x3fff) + 1)
    } else {
        if size < 10 {
            return Err("Truncated WebP lossy header".into());
        }
        let h = read::<10>(input, data.start)?;
        let tag = u24(&h[..3]);
        if tag & 1 != 0 || h[3..6] != [0x9d, 1, 0x2a] || u64::from(tag >> 5) > size - 10 {
            return Err("Invalid WebP VP8 frame header or partition size".into());
        }
        (
            u32::from(u16::from_le_bytes([h[6], h[7]]) & 0x3fff),
            u32::from(u16::from_le_bytes([h[8], h[9]]) & 0x3fff),
        )
    };
    Ok((w, h))
}

pub(crate) fn inspect(
    input: &mut (impl Read + Seek),
    length: u64,
    managed: bool,
    check: &dyn Fn() -> Result<(), String>,
) -> Result<Info, String> {
    inspect_with_limits(input, length, managed, CanvasLimits::Resident, check)
}

pub(super) fn inspect_native(
    input: &mut (impl Read + Seek),
    length: u64,
    managed: bool,
    check: &dyn Fn() -> Result<(), String>,
) -> Result<Info, String> {
    inspect_with_limits(input, length, managed, CanvasLimits::Native, check)
}

/// Inspect and bind chooser metadata in one monotonic pass, without staging the payload.
pub(super) fn inspect_held(
    input: &mut super::source_file::FileSource<'_>,
    check: &dyn Fn() -> Result<(), String>,
) -> Result<Info, String> {
    struct Forward<'a, 'source> {
        input: &'a mut super::source_file::FileSource<'source>,
        check: &'a dyn Fn() -> Result<(), String>,
    }
    impl Read for Forward<'_, '_> {
        fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
            self.input.read(bytes)
        }
    }
    impl Seek for Forward<'_, '_> {
        fn seek(&mut self, to: SeekFrom) -> io::Result<u64> {
            let current = self.input.stream_position()?;
            match to {
                SeekFrom::Start(position) if (current..=self.input.length()).contains(&position) => {
                    self.input.skip(position - current, &mut || (self.check)())
                        .map_err(io::Error::other)?;
                    Ok(position)
                }
                SeekFrom::Current(0) => Ok(current),
                _ => Err(io::Error::other("WebP inspection requested a non-forward seek")),
            }
        }
    }

    let length = input.length();
    if length > super::ENCODED_LIMIT as u64 {
        input.inspect_seekable(|file, length| preflight_large(file, length, true, check))?;
    } else {
        input.start_read()?;
    }
    let info = inspect_native(&mut Forward { input, check }, length, true, check)?;
    input.admit(&mut || check())?;
    Ok(info)
}

pub(crate) fn preflight_large(
    input: &mut (impl Read + Seek),
    length: u64,
    managed: bool,
    check: &dyn Fn() -> Result<(), String>,
) -> Result<(), String> {
    if length > super::ENCODED_LIMIT as u64 {
        inspect_native(input, length, managed, check)?;
    }
    Ok(())
}

fn check_encoded_layout(info: &Info, length: u64) -> Result<(), String> {
    if length > info.native_file_limit() {
        return Err("WebP encoded size exceeds admission for this container layout".into());
    }
    Ok(())
}

fn inspect_with_limits(
    input: &mut (impl Read + Seek),
    length: u64,
    managed: bool,
    limits: CanvasLimits,
    check: &dyn Fn() -> Result<(), String>,
) -> Result<Info, String> {
    check()?;
    let limit = match limits {
        CanvasLimits::Resident => super::ENCODED_LIMIT as u64,
        CanvasLimits::Native => FILE_LIMITS.encoded_bytes,
    };
    if length > limit || length < 20 {
        return Err("WebP encoded size exceeds admission".into());
    }
    let header = read::<12>(input, 0)?;
    if &header[..4] != b"RIFF"
        || &header[8..] != b"WEBP"
        || u64::from(u32::from_le_bytes(header[4..8].try_into().unwrap())) + 8 != length
    {
        return Err("Invalid WebP container size or signature".into());
    }
    let mut info = Info {
        width: 0,
        height: 0,
        frames: 0,
        animated: false,
        lossy: false,
        lossless: false,
        alpha_chunk: false,
        static_alpha: None,
        profile: None,
        exif: None,
    };
    let mut extended = None;
    let mut anim = false;
    let mut pixels = false;
    let mut alpha = false;
    let mut count = 0usize;
    let mut metadata = 0u64;
    let mut at = 12;
    while at < length {
        check()?;
        count += 1;
        if count > 65536 {
            return Err("WebP chunk count exceeds admission".into());
        }
        let (kind, data, next) = chunk(input, at, length)?;
        let size = data.end - data.start;
        match &kind {
            b"VP8X" => {
                if at != 12 || size != 10 {
                    return Err("Invalid WebP extended header".into());
                }
                let h = read::<10>(input, data.start)?;
                if h[0] & 0xc1 != 0 || h[1..4] != [0; 3] {
                    return Err("Unsupported WebP header flags".into());
                }
                extended = Some(h[0]);
                info.animated = h[0] & 2 != 0;
                info.width = u24(&h[4..7]) + 1;
                info.height = u24(&h[7..10]) + 1;
                if info.animated {
                    check_encoded_layout(&info, length)?;
                }
                limits.check(info.width, info.height)?;
            }
            b"ANIM" => {
                if !info.animated || anim || info.frames != 0 || size != 6 {
                    return Err("Invalid WebP animation header".into());
                }
                anim = true;
            }
            b"ANMF" => {
                if !anim || pixels || size < 32 {
                    return Err("Invalid WebP animation frame".into());
                }
                info.frames += 1;
                if info.frames > super::MAX_ITEMS {
                    return Err("WebP animation frame limit exceeded".into());
                }
                let h = read::<16>(input, data.start)?;
                let w = u24(&h[6..9]) + 1;
                let height = u24(&h[9..12]) + 1;
                if h[15] & !3 != 0
                    || u24(&h[..3]) * 2 + w > info.width
                    || u24(&h[3..6]) * 2 + height > info.height
                {
                    return Err("WebP frame exceeds its canvas or uses unsupported flags".into());
                }
                let mut part = data.start + 16;
                let mut has_pixels = false;
                let mut has_alpha = false;
                while part < data.end {
                    check()?;
                    count += 1;
                    if count > 65536 {
                        return Err("WebP chunk count exceeds admission".into());
                    }
                    let (code, payload, after) = chunk(input, part, data.end)?;
                    match &code {
                        b"ALPH" if !has_pixels && !has_alpha && payload.end > payload.start => {
                            has_alpha = true;
                            info.alpha_chunk = true;
                            info.lossless = true; // ALPH may contain a VP8L stream.
                        }
                        b"VP8 " | b"VP8L" if !has_pixels && !(has_alpha && &code == b"VP8L") => {
                            if dimensions(input, code, &payload)? != (w, height) {
                                return Err("WebP embedded frame dimensions differ".into());
                            }
                            has_pixels = true;
                            info.lossy |= &code == b"VP8 ";
                            info.lossless |= &code == b"VP8L";
                        }
                        b"ALPH" | b"VP8 " | b"VP8L" => {
                            return Err("Ambiguous WebP frame data".into())
                        }
                        _ if !has_pixels => return Err("Unsupported WebP frame chunk order".into()),
                        _ => {}
                    }
                    part = after;
                }
                if !has_pixels {
                    return Err("WebP frame has no image data".into());
                }
            }
            b"ALPH" => {
                if info.animated || extended.is_none() || alpha || pixels || size == 0 {
                    return Err("Invalid WebP alpha chunk".into());
                }
                alpha = true;
                info.alpha_chunk = true;
                let alpha_header = read::<1>(input, data.start)?[0];
                if matches!(limits, CanvasLimits::Native) && extended.is_some_and(|flags| flags & 16 != 0) {
                    if alpha_header & 0x30 > 0x10 {
                        return Err("Invalid WebP alpha preprocessing".into());
                    }
                    let compression = alpha_header & 3;
                    if compression > 1 {
                        return Err("Invalid WebP alpha compression".into());
                    }
                    if compression == 1 && size == 1 {
                        return Err("Truncated compressed WebP alpha plane".into());
                    }
                    if compression == 0 && size != 1 + u64::from(info.width) * u64::from(info.height) {
                        return Err("Uncompressed WebP alpha length differs from its canvas".into());
                    }
                    info.static_alpha = Some(StaticAlpha { range: data, header: alpha_header });
                    info.lossless = compression == 1;
                } else {
                    info.lossless = true;
                }
            }
            b"VP8 " | b"VP8L" => {
                if info.animated
                    || pixels
                    || (alpha && &kind == b"VP8L")
                    || (extended.is_none() && at != 12)
                {
                    return Err("Ambiguous WebP image data".into());
                }
                let size = dimensions(input, kind, &data)?;
                limits.check(size.0, size.1)?;
                if extended.is_some() && size != (info.width, info.height) {
                    return Err("WebP canvas dimensions differ".into());
                }
                (info.width, info.height) = size;
                pixels = true;
                info.lossy |= &kind == b"VP8 ";
                info.lossless |= &kind == b"VP8L";
                check_encoded_layout(&info, length)?;
            }
            _ => {
                metadata = metadata
                    .checked_add(size)
                    .ok_or("WebP metadata size overflow")?;
                if metadata > 8 * 1024 * 1024 {
                    return Err("WebP metadata exceeds admission".into());
                }
                if managed && &kind == b"ICCP" {
                    if info.profile.is_some()
                        || size == 0
                        || size > crate::color_profile::MAX_PROFILE_BYTES as u64
                    {
                        return Err("Ambiguous, empty or oversized WebP ICC profile".into());
                    }
                    info.profile = Some(data);
                } else if managed && &kind == b"EXIF" {
                    if info.exif.is_some() || size > 1024 * 1024 {
                        return Err("Ambiguous or oversized WebP EXIF metadata".into());
                    }
                    info.exif = Some(data);
                }
            }
        }
        at = next;
    }
    if info.animated && info.frames == 0 || !info.animated && !pixels {
        return Err("WebP has no image frames".into());
    }
    if managed
        && (info.profile.is_some() != extended.is_some_and(|f| f & 32 != 0)
            || info.exif.is_some() != extended.is_some_and(|f| f & 8 != 0))
    {
        return Err("WebP declared color/orientation metadata is missing or inconsistent".into());
    }
    info.frames = info.frames.max(1);
    check()?;
    Ok(info)
}

pub(super) fn inspect_file(path: &std::path::Path) -> Result<super::Info, String> {
    if !path.is_absolute() {
        return Err("Use an absolute local image path".into());
    }
    let mut file = crate::file_safety::open_native_read(path)
        .map_err(|e| format!("Cannot read image: {e}"))?;
    let length = file.metadata().map_err(|e| e.to_string())?.len();
    let info = inspect_native(&mut file, length, true, &|| Ok(()))?;
    Ok(super::Info {
        format: "WebP".into(), width: info.width, height: info.height, bit_depth: 8,
        choice: (info.frames > 1).then_some("frame"), count: info.frames,
    })
}

pub(crate) fn check_reader(input: &mut (impl Read + Seek)) -> Result<(), String> {
    let length = input.seek(SeekFrom::End(0)).map_err(|e| e.to_string())?;
    inspect(input, length, false, &|| Ok(()))?;
    input.seek(SeekFrom::Start(0)).map_err(|e| e.to_string())?;
    Ok(())
}

pub(super) fn profile<'a>(
    bytes: &'a [u8],
    check: &dyn Fn() -> Result<(), String>,
) -> Result<Option<&'a [u8]>, String> {
    let info = inspect(
        &mut std::io::Cursor::new(bytes),
        bytes.len() as u64,
        true,
        check,
    )?;
    Ok(info
        .profile
        .map(|range| &bytes[range.start as usize..range.end as usize]))
}

#[cfg(test)]
pub(crate) mod tests;
