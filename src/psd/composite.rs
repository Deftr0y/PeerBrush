//! Append-only native composite strips, replayed into the standard planar spool.
use super::*;
use ring::digest::{Context, Digest, SHA256};

pub(super) trait Scratch: Read + Write + Seek {}
impl<T: Read + Write + Seek> Scratch for T {}

const STRIP_PIXELS: usize = 128 * 1024;
const TRANSFER: usize = 64 * 1024;
struct Segment {
    offset: u64,
    bytes: usize,
    digest: Digest,
}

pub(super) fn write(
    doc: &Document,
    scratch: &mut dyn Scratch,
    output: &mut dyn Write,
    check: &dyn Fn() -> Result<(), String>,
    progress: &dyn Fn(&str, usize, usize) -> Result<(), String>,
) -> Result<(), String> {
    check()?;
    if scratch.stream_position().map_err(|e| e.to_string())? != 0
        || scratch.seek(SeekFrom::End(0)).map_err(|e| e.to_string())? != 0
    {
        return Err("Composite scratch must be empty at position zero".into());
    }
    let width = doc.width as usize;
    let rows = (STRIP_PIXELS / width).max(1);
    let bytes_per_sample = usize::from(doc.bit_depth / 8);
    let mut channels: [Vec<Segment>; 4] = Default::default();
    let mut offset = 0;
    let mut planar = Vec::new();
    progress("Rendering composite strips", 0, doc.height as usize)?;
    for top in (0..doc.height as usize).step_by(rows) {
        check()?;
        let bottom = (top + rows).min(doc.height as usize);
        let rect = [0, top as i32, doc.width as i32, bottom as i32];
        let (bytes, words) = if doc.bit_depth == 16 {
            let image = crate::depth16::render_crop(doc, rect)?;
            if image.width != doc.width || image.height as usize != bottom - top {
                return Err("Native composite strip dimensions changed".into());
            }
            (Vec::new(), image.words)
        } else {
            let (w, h, pixels, _) =
                doc.preview(Some(rect), doc.width.max(doc.height), None, false)?;
            if w != doc.width || h as usize != bottom - top {
                return Err("Composite strip dimensions changed".into());
            }
            (pixels, Vec::new())
        };
        let samples = width * (bottom - top);
        if (doc.bit_depth == 16 && words.len() != samples * 4)
            || (doc.bit_depth == 8 && bytes.len() != samples * 4)
        {
            return Err("Invalid composite strip length".into());
        }
        for (channel, segments) in channels.iter_mut().enumerate() {
            check()?;
            planar.clear();
            if doc.bit_depth == 16 {
                for pixel in words.chunks_exact(4) {
                    let value = if channel < 3 {
                        ((pixel[channel] as u64 * pixel[3] as u64
                            + 65535 * (65535 - pixel[3] as u64)
                            + 32767)
                            / 65535) as u16
                    } else {
                        pixel[3]
                    };
                    planar.extend_from_slice(&value.to_be_bytes());
                }
            } else {
                for pixel in bytes.chunks_exact(4) {
                    planar.push(if channel < 3 {
                        ((pixel[channel] as u32 * pixel[3] as u32
                            + 255 * (255 - pixel[3] as u32)
                            + 127)
                            / 255) as u8
                    } else {
                        pixel[3]
                    });
                }
            }
            for chunk in planar.chunks(TRANSFER) {
                check()?;
                scratch.write_all(chunk).map_err(|e| e.to_string())?;
            }
            segments.push(Segment {
                offset,
                bytes: planar.len(),
                digest: ring::digest::digest(&SHA256, &planar),
            });
            offset += planar.len() as u64;
        }
        progress("Rendering composite strips", bottom, doc.height as usize)?;
    }
    scratch.flush().map_err(|e| e.to_string())?;
    if offset != u64::from(doc.width) * u64::from(doc.height) * 4 * bytes_per_sample as u64
        || scratch.seek(SeekFrom::End(0)).map_err(|e| e.to_string())? != offset
    {
        return Err("Composite scratch length changed".into());
    }
    drop(planar);
    let mut transfer = vec![0; TRANSFER];
    let mut completed = 0;
    for channel in channels {
        for segment in channel {
            check()?;
            scratch
                .seek(SeekFrom::Start(segment.offset))
                .map_err(|e| e.to_string())?;
            let mut remaining = segment.bytes;
            let mut hash = Context::new(&SHA256);
            while remaining != 0 {
                check()?;
                let length = remaining.min(TRANSFER);
                scratch
                    .read_exact(&mut transfer[..length])
                    .map_err(|e| e.to_string())?;
                hash.update(&transfer[..length]);
                output
                    .write_all(&transfer[..length])
                    .map_err(|e| e.to_string())?;
                remaining -= length;
            }
            if hash.finish().as_ref() != segment.digest.as_ref() {
                return Err("Composite scratch changed during replay".into());
            }
            completed += segment.bytes / (width * bytes_per_sample);
            progress(
                "Writing current composite",
                completed,
                doc.height as usize * 4,
            )?;
        }
    }
    check()?;
    if scratch.seek(SeekFrom::End(0)).map_err(|e| e.to_string())? != offset {
        return Err("Composite scratch length changed".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    struct Probe {
        data: Cursor<Vec<u8>>,
        fault: &'static str,
        reading: bool,
        writes: Vec<usize>,
    }
    impl Write for Probe {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            assert!(!self.reading);
            assert_eq!(self.data.position(), self.data.get_ref().len() as u64);
            self.writes.push(bytes.len());
            if self.fault == "write" {
                return Err(std::io::Error::other("scratch write"));
            }
            self.data.write(bytes)
        }
        fn flush(&mut self) -> std::io::Result<()> {
            if self.fault == "flush" {
                return Err(std::io::Error::other("scratch flush"));
            }
            Ok(())
        }
    }
    impl Read for Probe {
        fn read(&mut self, bytes: &mut [u8]) -> std::io::Result<usize> {
            self.reading = true;
            if self.fault == "read" {
                return Err(std::io::Error::other("scratch read"));
            }
            let count = self.data.read(bytes)?;
            if self.fault == "corrupt" && count > 0 {
                bytes[0] ^= 1;
            }
            Ok(count)
        }
    }
    impl Seek for Probe {
        fn seek(&mut self, from: SeekFrom) -> std::io::Result<u64> {
            self.data.seek(from)
        }
    }
    fn fixture(depth: u16) -> Document {
        let mut doc = Document::new_depth(257, 515, depth).unwrap();
        let words: Vec<u16> = (0..257 * 515 * 4)
            .map(|n| (n as u16).wrapping_mul(971).wrapping_add(91))
            .collect();
        doc.layers[0].pixels = if depth == 16 {
            Raster::from_rgba16(257, 515, &words).unwrap()
        } else {
            Raster::from_rgba(
                257,
                515,
                &words
                    .iter()
                    .map(|n| crate::raster::project16(*n))
                    .collect::<Vec<_>>(),
            )
            .unwrap()
        };
        doc
    }
    #[test]
    fn multistrip_native_psd_psb_matches_materialized_standard_and_editable_sources() {
        for depth in [8, 16] {
            let doc = fixture(depth);
            let before = serde_json::to_value(&doc).unwrap();
            for layout in [Layout::Psd, Layout::Psb] {
                let reference = encode_layout(&doc, layout).unwrap();
                let mut scratch = Probe {
                    data: Cursor::new(Vec::new()),
                    fault: "none",
                    reading: false,
                    writes: vec![],
                };
                let mut output = Vec::new();
                encode_layout_scratch_to(
                    &doc,
                    layout,
                    &mut output,
                    &mut Cursor::new(Vec::new()),
                    Some(&mut scratch),
                    &|| Ok(()),
                    &|_, _, _| Ok(()),
                )
                .unwrap();
                assert_eq!(
                    output, reference,
                    "standard bytes, private sources and composite must match"
                );
                assert_eq!(
                    scratch.data.get_ref().len(),
                    257 * 515 * 4 * usize::from(depth / 8)
                );
                assert!(scratch.writes.iter().all(|n| *n <= TRANSFER));
                assert!(scratch.writes.len() > 4, "must use multiple strips");
                assert_eq!(
                    serde_json::to_value(decode(&output).unwrap()).unwrap(),
                    before
                );
            }
        }
    }
    #[test]
    fn scratch_faults_and_midstrip_cancellation_preserve_the_document() {
        for depth in [8, 16] {
            let doc = fixture(depth);
            let before = serde_json::to_value(&doc).unwrap();
            for fault in ["write", "flush", "read", "corrupt", "cancel"] {
                let mut scratch = Probe {
                    data: Cursor::new(Vec::new()),
                    fault,
                    reading: false,
                    writes: vec![],
                };
                let mut output = Vec::new();
                let result = encode_layout_scratch_to(
                    &doc,
                    Layout::Psb,
                    &mut output,
                    &mut Cursor::new(Vec::new()),
                    Some(&mut scratch),
                    &|| Ok(()),
                    &|stage, completed, _| {
                        if fault == "cancel"
                            && stage == "Rendering composite strips"
                            && completed > 0
                        {
                            Err("canceled".into())
                        } else {
                            Ok(())
                        }
                    },
                );
                assert!(result.is_err(), "{depth} {fault}");
                assert!(
                    output.is_empty(),
                    "failed scratch must never reach final output"
                );
                assert_eq!(serde_json::to_value(&doc).unwrap(), before);
            }
        }
    }
}
