//! Shared bounded container inspection before resident or native GIF decoding.
use std::io::Read;
use crate::raster::CanvasLimits;
mod native;
pub(super) use native::{prepare_source, FILE_LIMITS};
pub(crate) use native::{prepare, prepare_patch};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Info {
    pub width: u32,
    pub height: u32,
    pub frames: usize,
}

pub(crate) fn inspect(
    input: &mut impl Read,
    check: &dyn Fn() -> Result<(), String>,
) -> Result<Info, String> {
    inspect_inner(input, CanvasLimits::Resident, true, check)
}

pub(super) fn inspect_native(
    input: &mut impl Read,
    color_managed: bool,
    check: &dyn Fn() -> Result<(), String>,
) -> Result<Info, String> {
    inspect_inner(input, CanvasLimits::Native, color_managed, check)
}

pub(super) fn inspect_file(path: &std::path::Path) -> Result<super::Info, String> {
    if !path.is_absolute() {
        return Err("Use an absolute local image path".into());
    }
    let file = crate::file_safety::open_native_read(path).map_err(|e| format!("Cannot read image: {e}"))?;
    let length = file.metadata().map_err(|e| e.to_string())?.len();
    if length > FILE_LIMITS.encoded_bytes {
        return Err("Encoded GIF image must be a file at most 128 MiB".into());
    }
    let mut reader = std::io::BufReader::with_capacity(64 * 1024, file.take(length));
    let info = inspect_native(&mut reader, true, &|| Ok(()))?;
    Ok(super::Info {
        format: "Gif".into(), width: info.width, height: info.height, bit_depth: 8,
        choice: (info.frames > 1).then_some("frame"), count: info.frames,
    })
}

fn inspect_inner(
    input: &mut impl Read,
    limits: CanvasLimits,
    color_managed: bool,
    check: &dyn Fn() -> Result<(), String>,
) -> Result<Info, String> {
    fn read(
        input: &mut impl Read,
        bytes: &mut [u8],
        check: &dyn Fn() -> Result<(), String>,
    ) -> Result<(), String> {
        check()?;
        input
            .read_exact(bytes)
            .map_err(|e| format!("Invalid GIF input: {e}"))?;
        check()
    }
    fn byte(input: &mut impl Read, check: &dyn Fn() -> Result<(), String>) -> Result<u8, String> {
        let mut value = [0];
        read(input, &mut value, check)?;
        Ok(value[0])
    }
    fn palette(
        input: &mut impl Read,
        packed: u8,
        check: &dyn Fn() -> Result<(), String>,
    ) -> Result<(), String> {
        if packed & 128 != 0 {
            let mut table = [0; 768];
            read(input, &mut table[..3 * (1 << ((packed & 7) + 1))], check)?;
        }
        Ok(())
    }
    let mut header = [0; 13];
    read(input, &mut header, check)?;
    if !matches!(&header[..6], b"GIF87a" | b"GIF89a") {
        return Err("Invalid GIF signature".into());
    }
    let mut info = Info {
        width: u16::from_le_bytes([header[6], header[7]]) as u32,
        height: u16::from_le_bytes([header[8], header[9]]) as u32,
        frames: 0,
    };
    limits.check(info.width, info.height)?;
    palette(input, header[10], check)?;
    loop {
        let application = match byte(input, check)? {
            0x3b => break,
            0x21 => byte(input, check)? == 0xff,
            0x2c => {
                let mut descriptor = [0; 9];
                read(input, &mut descriptor, check)?;
                palette(input, descriptor[8], check)?;
                byte(input, check)?; // LZW minimum code size; decoder validates its semantics.
                info.frames += 1;
                if info.frames > super::MAX_ITEMS {
                    return Err("GIF animation frame limit exceeded".into());
                }
                false
            }
            _ => return Err("Invalid GIF block".into()),
        };
        let mut first = true;
        loop {
            let length = byte(input, check)? as usize;
            if length == 0 {
                break;
            }
            let mut data = [0; 255];
            read(input, &mut data[..length], check)?;
            if color_managed && application && first && &data[..length] == b"ICCRGBG1012" {
                return Err(
                    "GIF ICC color extensions require explicit conversion before importing".into(),
                );
            }
            first = false;
        }
    }
    if info.frames == 0 {
        return Err("GIF contains no frames".into());
    }
    check()?;
    Ok(info)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        cell::Cell,
        io::{self, Cursor},
    };

    fn fixture(frames: usize) -> Vec<u8> {
        let mut bytes = b"GIF89a\x01\0\x01\0\x80\0\0\0\0\0\xff\xff\xff".to_vec();
        for _ in 0..frames {
            // One white pixel, LZW minimum code size 2, then data terminator.
            bytes.extend_from_slice(b"\x2c\0\0\0\0\x01\0\x01\0\0\x02\x02\x4c\x01\0");
        }
        bytes.push(0x3b);
        bytes
    }
    struct ShortReads<'a>(&'a mut Cursor<Vec<u8>>);
    impl Read for ShortReads<'_> {
        fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
            let count = out.len().min(1);
            self.0.read(&mut out[..count])
        }
    }

    #[test]
    fn inspection_counts_real_frames_with_bounded_short_reads_and_preserves_decode() {
        for count in [1, 3, super::super::MAX_ITEMS] {
            let bytes = fixture(count);
            let mut input = Cursor::new(bytes.clone());
            assert_eq!(
                inspect(&mut ShortReads(&mut input), &|| Ok(())).unwrap(),
                Info {
                    width: 1,
                    height: 1,
                    frames: count
                }
            );
            assert_eq!(input.position(), bytes.len() as u64);
            let encoded = super::super::Encoded::new(bytes, image::ImageFormat::Gif).unwrap();
            assert_eq!(encoded.info.count, count);
            let pixels = encoded
                .decode(&serde_json::json!({"frame":count-1}), super::super::BUDGET)
                .unwrap();
            assert_eq!(pixels.rgba(), [255, 255, 255, 255]);
        }
    }

    #[test]
    fn inspection_rejects_icc_truncation_empty_frames_and_excess_frames() {
        let bytes = fixture(1);
        for end in 0..bytes.len() {
            assert!(inspect(&mut Cursor::new(&bytes[..end]), &|| Ok(())).is_err());
        }
        for count in [0, super::super::MAX_ITEMS + 1] {
            assert!(inspect(&mut Cursor::new(fixture(count)), &|| Ok(())).is_err());
        }
        let mut profile = bytes[..19].to_vec();
        profile.extend_from_slice(b"\x21\xff\x0bICCRGBG1012\x02\x01\x01\0");
        profile.extend_from_slice(&bytes[19..]);
        assert!(inspect(&mut Cursor::new(profile), &|| Ok(()))
            .unwrap_err()
            .contains("ICC"));
    }

    #[test]
    fn native_gif_inspection_accepts_large_static_and_animated_canvas_geometry() {
        let edge = crate::raster::NATIVE_EDGE as u16;
        for (width, height, frames, accepted) in [
            (edge, edge, 1, true),
            (edge, edge, 2, true),
            (edge + 1, 1, 2, false),
        ] {
            let mut bytes = fixture(frames);
            bytes[6..8].copy_from_slice(&width.to_le_bytes());
            bytes[8..10].copy_from_slice(&height.to_le_bytes());
            assert!(inspect(&mut Cursor::new(&bytes), &|| Ok(())).is_err());
            for color_managed in [false, true] {
                let info = inspect_native(&mut Cursor::new(&bytes), color_managed, &|| Ok(()));
                if accepted {
                    assert_eq!(info.unwrap(), Info { width: u32::from(width), height: u32::from(height), frames });
                } else {
                    assert!(info.is_err());
                }
            }
        }
    }

    #[test]
    fn inspection_checks_cancellation_inside_extension_subblocks() {
        let bytes = fixture(1);
        let mut comments = bytes[..19].to_vec();
        comments.extend_from_slice(b"\x21\xfe");
        for _ in 0..64 {
            comments.extend_from_slice(&[1, b'x']);
        }
        comments.push(0);
        comments.extend_from_slice(&bytes[19..]);
        let calls = Cell::new(0);
        let reason = inspect(&mut Cursor::new(comments), &|| {
            calls.set(calls.get() + 1);
            if calls.get() == 20 {
                Err("GIF inspection interrupted".into())
            } else {
                Ok(())
            }
        })
        .unwrap_err();
        assert_eq!(reason, "GIF inspection interrupted");
    }
}
