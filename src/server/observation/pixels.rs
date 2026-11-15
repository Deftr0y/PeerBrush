//! PNG transport for the shared renderer's complete display-only frame.
use super::super::reply::{SegmentWriter, Segments};
use crate::{
    large_document::ResidentBudget,
    render::native_preview::{Frame, Tag},
};
use std::io::Write;
type Result<T> = std::result::Result<T, String>;

pub(in crate::server) struct Pixels(Frame);
impl Pixels {
    pub(in crate::server) fn new(frame: Frame) -> Self {
        Self(frame)
    }
    pub(in crate::server) fn encode(
        self,
        pool: &ResidentBudget,
        check: &mut dyn FnMut() -> Result<()>,
    ) -> Result<(Segments, Tag)> {
        let tag = self.0.tag();
        let key = tag.key();
        let width = key.width;
        let height = key.height;
        // Same pinned png0.18 backend envelope used by native export, without
        // profile bytes because these are already converted display samples.
        let backend = width as usize * 4 * 8 + 4 * 1024 * 1024;
        let _backend = pool
            .reserve_bytes(backend, 32 * 1024 * 1024)
            .map_err(|e| e.to_string())?;
        let raw = width as usize * height as usize * 4;
        let mut output = SegmentWriter::new(
            pool,
            raw.checked_add(height as usize * 16 + 1024 * 1024)
                .ok_or("Preview PNG length overflow")?,
            check,
        )?;
        let mut encoder = png::Encoder::new(&mut output, width, height);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        encoder.set_compression(png::Compression::Fast);
        encoder.set_filter(png::Filter::Adaptive);
        let mut writer = encoder.write_header().map_err(|e| e.to_string())?;
        {
            let mut stream = writer
                .stream_writer_with_size(64 * 1024)
                .map_err(|e| e.to_string())?;
            let mut first = 0;
            let mut stripe = |rows: [u32; 2], bytes: &[u8]| -> Result<()> {
                if rows[0] != first
                    || rows[1] <= rows[0]
                    || bytes.len() != (rows[1] - rows[0]) as usize * width as usize * 4
                {
                    return Err("Preview PNG rows are incomplete".into());
                }
                stream.write_all(bytes).map_err(|e| e.to_string())?;
                first = rows[1];
                Ok(())
            };
            for value in self.0.stripes() {
                stripe(value.rows(), value.bytes())?;
            }
            if first != height {
                return Err("Preview PNG height is incomplete".into());
            }
            stream.finish().map_err(|e| e.to_string())?;
        }
        writer.finish().map_err(|e| e.to_string())?;
        drop(self);
        Ok((output.finish()?, tag))
    }
}
