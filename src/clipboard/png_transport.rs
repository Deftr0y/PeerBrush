use super::Image;
use crate::{large_document::ResidentBudget, psd::memory36::MetadataCharge};
use std::{io::{self, Write}, sync::Arc};

pub(super) const LIMIT: usize = 128 * 1024 * 1024;

pub(super) struct Encoded {
    bytes: Vec<u8>,
    _charge: Option<Arc<MetadataCharge>>,
}
impl Encoded {
    #[cfg(windows)]
    pub(super) fn bytes(&self) -> &[u8] {
        &self.bytes
    }
}

struct Output<'a> {
    encoded: Encoded,
    limit: usize,
    budget: Option<&'a ResidentBudget>,
    check: &'a dyn Fn() -> Result<(), String>,
}

impl Write for Output<'_> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let end = self
            .encoded.bytes
            .len()
            .checked_add(bytes.len())
            .filter(|&end| end <= self.limit)
            .ok_or_else(|| io::Error::other("Clipboard PNG exceeds 128 MiB"))?;
        if end > self.encoded.bytes.capacity() {
            let capacity = end.next_power_of_two().min(self.limit);
            // A moving reallocation can briefly hold the old and new buffers.
            let charge = self.budget.map(|budget| MetadataCharge::reserve(budget, capacity, self.check))
                .transpose().map_err(io::Error::other)?;
            self.encoded.bytes
                .try_reserve_exact(capacity - self.encoded.bytes.len())
                .map_err(|_| io::Error::other("Clipboard PNG allocation refused"))?;
            self.encoded._charge = charge;
            if self.encoded.bytes.capacity() > capacity {
                return Err(io::Error::other("Clipboard PNG allocation exceeds admission"));
            }
        }
        self.encoded.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

pub(super) fn encode(
    image: &Image,
    identity: Option<[u8; 16]>,
    limit: usize,
    check: &dyn Fn() -> Result<(), String>,
) -> Result<Vec<u8>, String> {
    Ok(encode_admitted(image, identity, limit, None, check)?.bytes)
}

pub(super) fn encode_admitted(
    image: &Image,
    identity: Option<[u8; 16]>,
    limit: usize,
    budget: Option<&ResidentBudget>,
    check: &dyn Fn() -> Result<(), String>,
) -> Result<Encoded, String> {
    check()?;
    image.validate()?;
    // Streaming uses bounded filter rows, a compressor and one IDAT chunk.
    let _scratch = budget.map(|budget| MetadataCharge::reserve(budget, 16 * 1024 * 1024, check)).transpose()?;
    let mut output = Output {
        encoded: Encoded { bytes: Vec::new(), _charge: None },
        limit,
        budget,
        check,
    };
    let active = || check().map_err(io::Error::other);
    (|| -> Result<(), png::EncodingError> {
        let mut encoder = png::Encoder::new(&mut output, image.width, image.height);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(if image.samples16.is_some() {
            png::BitDepth::Sixteen
        } else {
            png::BitDepth::Eight
        });
        encoder.set_compression(png::Compression::Fast);
        encoder.set_filter(png::Filter::Adaptive);
        let mut writer = encoder.write_header()?;
        if let Some(identity) = identity {
            // Private ancillary chunk, unsafe to retain across pixel edits.
            writer.write_chunk(png::chunk::ChunkType(*b"pbID"), &identity)?;
        }
        let mut stream = writer.stream_writer_with_size(64 * 1024)?;
        let channels = image.width as usize * 4;
        if let Some(words) = &image.samples16 {
            let mut row = vec![0; channels * 2];
            for words in words.chunks_exact(channels) {
                active()?;
                for (bytes, &word) in row.chunks_exact_mut(2).zip(words) {
                    bytes.copy_from_slice(&word.to_be_bytes());
                }
                stream.write_all(&row)?;
            }
        } else {
            for row in image.bytes.chunks_exact(channels) {
                active()?;
                stream.write_all(row)?;
            }
        }
        active()?;
        stream.finish()?;
        writer.finish()
    })()
    .map_err(|e| e.to_string())?;
    Ok(output.encoded)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encoded_growth_needs_room_for_both_buffers_and_refunds_on_drop() {
        let budget = ResidentBudget::new(16).unwrap();
        let mut output = Output {
            encoded: Encoded { bytes: Vec::new(), _charge: None },
            limit: 64,
            budget: Some(&budget),
            check: &|| Ok(()),
        };
        output.write_all(&[17; 8]).unwrap();
        assert_eq!(budget.used(), 8);
        assert!(output.write_all(&[31]).is_err());
        assert_eq!(output.encoded.bytes, [17; 8]);
        assert_eq!(budget.used(), 8);
        drop(output);
        assert_eq!(budget.used(), 0);
    }

    #[test]
    fn tagged_rows_preserve_exact_samples_and_fit_only_within_the_output_limit() {
        let words = [12347, 33559, 51237, 0, 54321, 10987, 32109, 49153].repeat(700);
        let display = words
            .iter()
            .copied()
            .map(crate::raster::project16)
            .collect::<Vec<_>>();
        for samples16 in [None, Some(words)] {
            let image = Image {
                width: 70,
                height: 20,
                bytes: display.clone(),
                samples16,
                origin: None,
            };
            let identity = Some([5; 16]);
            let active = || Ok(());
            let bytes = encode(&image, identity, LIMIT, &active).unwrap();
            let exact = encode(&image, identity, bytes.len(), &active).unwrap();
            assert_eq!(exact, bytes);
            assert!(exact.capacity() <= bytes.len());
            assert!(encode(&image, identity, bytes.len() - 1, &active)
                .unwrap_err()
                .contains("Clipboard PNG exceeds"));
            let checks = std::cell::Cell::new(0);
            let interrupted = || {
                checks.set(checks.get() + 1);
                if checks.get() == 4 {
                    Err("Clipboard encoding cancelled".into())
                } else {
                    Ok(())
                }
            };
            assert!(encode(&image, identity, LIMIT, &interrupted)
                .unwrap_err()
                .contains("Clipboard encoding cancelled"));
            let restored = Image::png_bytes(exact).unwrap();
            assert_eq!(
                [restored.width, restored.height],
                [image.width, image.height]
            );
            assert_eq!(restored.samples16, image.samples16);
            assert_eq!(restored.bytes, image.bytes);
        }
    }
}
