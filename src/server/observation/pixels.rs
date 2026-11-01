//! Selection display is a derived RGBA8 projection, never an editable copy.
use super::super::reply::{SegmentWriter, Segments};
use crate::{
    engine::native::NativeDocument,
    large_document::{PinnedBytes, Reservation, ResidentBudget, ScratchBytes},
    raster::backed::Region,
    render::native_preview::{Frame, Tag},
    transform::native::{
        selected::{PreparedSelection, Selection},
        Work,
    },
};
use std::io::Write;
type Result<T> = std::result::Result<T, String>;
struct Stripe {
    rows: [u32; 2],
    bytes: PinnedBytes,
}
pub(super) enum Pixels {
    Frame(Frame),
    Selected(Selected),
}
pub(super) struct Selected {
    stripes: Vec<Stripe>,
    tag: Tag,
    _index: Reservation,
}
impl Pixels {
    pub(super) fn frame(
        frame: Frame,
        document: &NativeDocument,
        mode: Option<&str>,
        check: &dyn Fn() -> Result<()>,
    ) -> Result<Self> {
        if frame.key().source != document.key() {
            return Err("Selection preview belongs to another source generation".into());
        }
        if let Some(mode) = mode {
            if !["cutout", "mask", "overlay"].contains(&mode) {
                return Err("Unknown selection view".into());
            }
        }
        if mode.is_none() || document.metadata().selection().is_none() {
            return Ok(Self::Frame(frame));
        }
        let mode = mode.unwrap();
        let selection = Selection::for_folder(document)?;
        let key = frame.key();
        let scale = (key.width as f32 / (key.rect[2] - key.rect[0]) as f32)
            .min(key.height as f32 / (key.rect[3] - key.rect[1]) as f32);
        let x = |x: u32| key.rect[0] + (x as f32 / scale) as i32;
        let y = |y: u32| key.rect[1] + (y as f32 / scale) as i32;
        let pool = document.budget();
        let length = frame.stripes().len();
        let index = pool
            .reserve_bytes(
                length * std::mem::size_of::<Stripe>() + 128,
                4 * 1024 * 1024,
            )
            .map_err(|e| e.to_string())?;
        let mut stripes = Vec::new();
        stripes
            .try_reserve_exact(length)
            .map_err(|_| "Selection display index allocation refused")?;
        if stripes.capacity() * std::mem::size_of::<Stripe>() > index.bytes() {
            return Err("Selection display capacity exceeds admission".into());
        }
        let mut work = Work::new(64 * 1024 * 1024)?;
        let mut band: Option<Band<'_>> = None;
        for stripe in frame.stripes() {
            check()?;
            let mut bytes = ScratchBytes::new(pool, stripe.bytes().len(), 64 * 1024 * 1024)
                .map_err(|e| e.to_string())?;
            bytes.copy_from_slice(stripe.bytes());
            for row in stripe.rows()[0]..stripe.rows()[1] {
                check()?;
                let sy = y(row);
                let top =
                    selection.origin()[1] + (sy - selection.origin()[1]).div_euclid(256) * 256;
                if band.as_ref().is_none_or(|b| b.top != top) {
                    drop(band.take());
                    band = Some(Band::prepare(
                        &selection,
                        [x(0), top, x(key.width - 1) + 1, top + 256],
                        check,
                    )?);
                }
                let band = band.as_ref().unwrap();
                for column in 0..key.width {
                    let sx = x(column);
                    let byte = band.byte(sx, sy, &mut work)?;
                    let at = (((row - stripe.rows()[0]) * key.width + column) * 4) as usize;
                    let pixel = bytes[at..at + 4].try_into().unwrap();
                    bytes[at..at + 4].copy_from_slice(&project(pixel, byte, mode));
                }
            }
            stripes.push(Stripe {
                rows: stripe.rows(),
                bytes: bytes.freeze(),
            });
        }
        check()?;
        let tag = frame.tag();
        drop(band);
        drop(frame);
        Ok(Self::Selected(Selected {
            stripes,
            tag,
            _index: index,
        }))
    }
    fn tag(&self) -> Tag {
        match self {
            Self::Frame(frame) => frame.tag(),
            Self::Selected(value) => value.tag.clone(),
        }
    }
    pub(super) fn encode(
        self,
        pool: &ResidentBudget,
        check: &mut dyn FnMut() -> Result<()>,
    ) -> Result<(Segments, Tag)> {
        let tag = self.tag();
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
            match &self {
                Self::Frame(frame) => {
                    for value in frame.stripes() {
                        stripe(value.rows(), value.bytes())?;
                    }
                }
                Self::Selected(value) => {
                    for value in &value.stripes {
                        stripe(value.rows, &value.bytes)?;
                    }
                }
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
fn project(pixel: [u8; 4], byte: u8, mode: &str) -> [u8; 4] {
    let value = (byte as f32 / 255.).clamp(0., 1.);
    match mode {
        "mask" => {
            let v = (value * 255.).round() as u8;
            [v, v, v, 255]
        }
        "overlay" => crate::raster::blend(
            pixel,
            [233, 84, 32, ((1. - value) * 140.).round() as u8],
            1.,
            "normal",
        ),
        _ => [
            pixel[0],
            pixel[1],
            pixel[2],
            (pixel[3] as f32 * value).round() as u8,
        ],
    }
}
struct Band<'a> {
    columns: Vec<PreparedSelection<'a>>,
    left: i32,
    top: i32,
    _index: Reservation,
}
impl<'a> Band<'a> {
    fn prepare(
        selection: &'a Selection<'a>,
        bounds: [i32; 4],
        check: &dyn Fn() -> Result<()>,
    ) -> Result<Self> {
        let left =
            selection.origin()[0] + (bounds[0] - selection.origin()[0]).div_euclid(512) * 512;
        let count = ((bounds[2] - left) as usize).div_ceil(512);
        let bytes = count * std::mem::size_of::<PreparedSelection<'a>>() + 128;
        let index = selection
            .document()
            .budget()
            .reserve_bytes(bytes, 4 * 1024 * 1024)
            .map_err(|e| e.to_string())?;
        let mut columns = Vec::new();
        columns
            .try_reserve_exact(count)
            .map_err(|_| "Selection preview band allocation refused")?;
        if columns.capacity() * std::mem::size_of::<PreparedSelection<'a>>() > index.bytes() {
            return Err("Selection preview band exceeds admission".into());
        }
        for column in 0..count {
            check()?;
            let x = left + column as i32 * 512;
            let region =
                Region::new([x, bounds[1], x + 512, bounds[3]]).map_err(|e| e.to_string())?;
            let failure = std::cell::RefCell::new(None);
            let view = selection.prepare(region, &|| {
                check().map_err(|reason| {
                    *failure.borrow_mut() = Some(reason);
                    crate::large_document::Error::Cancelled
                })
            });
            let reason = failure.into_inner();
            columns.push(view.map_err(|e| reason.unwrap_or_else(|| e.to_string()))?);
        }
        Ok(Self {
            columns,
            left,
            top: bounds[1],
            _index: index,
        })
    }
    fn byte(&self, x: i32, y: i32, work: &mut Work) -> Result<u8> {
        let index = usize::try_from((x - self.left).div_euclid(512))
            .map_err(|_| "Selection display sample exceeds its prepared band")?;
        self.columns
            .get(index)
            .ok_or("Selection display column exceeds its prepared band")?
            .byte(x, y, work)
    }
}
