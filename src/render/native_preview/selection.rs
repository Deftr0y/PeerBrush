//! Shared, display-only selection projection over admitted native stripes.
//! A failed projection publishes no partial frame and never edits source data.
use super::{Key, Prepared};
use crate::{
    engine::scene::SceneSnapshot,
    large_document::Reservation,
    raster::backed::Region,
    transform::native::{
        selected::{PreparedSelection, Selection},
        Work,
    },
    workspace::source_runtime::ReadChecks,
};
use std::ops::Range;
type Result<T> = std::result::Result<T, String>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SelectionView {
    Cutout,
    Mask,
    Overlay,
}
impl SelectionView {
    pub(super) fn parse(mode: &str) -> Result<Self> {
        match mode {
            "cutout" => Ok(Self::Cutout),
            "mask" => Ok(Self::Mask),
            "overlay" => Ok(Self::Overlay),
            _ => Err("Unknown selection view".into()),
        }
    }
}
pub(super) fn capture<'a>(scene: &'a SceneSnapshot, key: &Key) -> Result<Option<Selection<'a>>> {
    if key.selection_view.is_none() {
        return Ok(None);
    }
    let document = scene.native_document()?;
    if key.source != document.key() {
        return Err("Selection preview belongs to another source generation".into());
    }
    if document.metadata().selection().is_none() {
        return Ok(None);
    }
    Selection::for_folder(document).map(Some)
}

pub(super) struct Projector<'a> {
    selection: &'a Selection<'a>,
    prepared: &'a Prepared,
    mode: SelectionView,
    work: Work,
    band: Option<Band<'a>>,
}
impl<'a> Projector<'a> {
    pub(super) fn new(selection: &'a Selection<'a>, prepared: &'a Prepared) -> Result<Self> {
        let key = &prepared.key;
        let mode = key.selection_view.ok_or("Selection preview mode is missing")?;
        // A streamed full canvas needs one coverage sample per output pixel.
        // Complex geometry still consumes the same explicit work allowance.
        let work = (64 * 1024 * 1024).max(u64::from(key.width) * u64::from(key.height));
        Ok(Self { selection, prepared, mode, work: Work::new(work)?, band: None })
    }
    /// Project before the renderer freezes the stripe; no duplicate display
    /// allocation or partially projected frame can escape to a consumer.
    pub(super) fn apply(
        &mut self,
        rows: [u32; 2],
        bytes: &mut [u8],
        checks: &mut ReadChecks<'_>,
    ) -> Result<()> {
        checks.pinned()?;
        let key = &self.prepared.key;
        let x = |x| self.prepared.x(x);
        let y = |y| self.prepared.y(y);
        if rows[0] >= rows[1] || rows[1] > key.height
            || bytes.len() != (rows[1] - rows[0]) as usize * key.width as usize * 4
        {
            return Err("Selection preview stripe footprint is inconsistent".into());
        }
        for row in rows[0]..rows[1] {
            checks.pinned()?;
            let sy = y(row);
            let top = self.selection.origin()[1]
                + (sy - self.selection.origin()[1]).div_euclid(256) * 256;
            if self.band.as_ref().is_none_or(|b| b.top != top) {
                drop(self.band.take());
                self.band = Some(Band::prepare(self.selection, top, key.width, &x, checks)?);
            }
            let band = self.band.as_ref().unwrap();
            let offset = ((row - rows[0]) * key.width * 4) as usize;
            for (columns, coverage) in &band.columns {
                let from = offset + columns.start as usize * 4;
                let to = offset + columns.end as usize * 4;
                let pixels = bytes[from..to].chunks_exact_mut(4);
                if coverage.is_none() && self.mode == SelectionView::Mask {
                    for pixel in pixels {
                        pixel.copy_from_slice(&[0, 0, 0, 255]);
                    }
                    continue;
                }
                for (column, pixel) in columns.clone().zip(pixels) {
                    let byte = coverage.as_ref().map_or(Ok(0), |prepared| {
                        prepared.byte(x(column), sy, &mut self.work)
                    })?;
                    let input = (&*pixel).try_into().unwrap();
                    pixel.copy_from_slice(&project(input, byte, self.mode));
                }
            }
        }
        checks.pinned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{engine::Document, raster::Raster, render::native_preview::Prepared};

    #[test]
    fn sparse_mask_projection_skips_empty_columns_on_exact_and_reduced_grids() {
        let mut doc = Document::new_depth(2051, 3, 16).unwrap();
        let bounds = [12, 1, 13, 2];
        let mut mask = Raster::new(2040, 1);
        mask.set(5, 0, [91; 4]);
        // Raw coverage outside the bounds hint is still sampled.
        mask.set(2033, 0, [173; 4]);
        doc.selection = Some(bounds);
        doc.selection_coverage = Some(crate::selection::Coverage {
            mask, bounds, origin: [7, 1], contours: vec![],
        });
        let (scene, path) = crate::compositor::native::tests::capture_kind(&doc, true);
        let rect = [2, 1, 2050, 2];
        for edge in [8192, 1024] {
            let prepared = Prepared::capture(
                &scene, Some(rect), edge, None, false, Some(SelectionView::Mask), &mut || Ok(()),
            ).unwrap();
            let key = &prepared.key;
            let selection = capture(&scene, key).unwrap().unwrap();
            let mut projector = Projector::new(&selection, &prepared).unwrap();
            projector.work = Work::new(u64::from(key.width) * 3 / 4).unwrap();
            let mut bytes = vec![255; key.width as usize * 4];
            let mut allow = || Ok(());
            let mut checks = ReadChecks::caller_only(&mut allow);
            projector.apply([0, 1], &mut bytes, &mut checks).unwrap();
            let mut expected = vec![0; bytes.len()];
            crate::selection::display::apply_region(&doc, key.width, 1, &mut expected, rect, "mask");
            assert_eq!(bytes, expected);
            assert!(bytes.chunks_exact(4).any(|p| p == [91, 91, 91, 255]));
            assert!(bytes.chunks_exact(4).any(|p| p == [173, 173, 173, 255]));
            assert!(bytes.chunks_exact(4).any(|p| p == [0, 0, 0, 255]));
        }
        drop(scene);
        crate::compositor::native::tests::cleanup(&path);
    }

    #[test]
    fn selection_projection_stays_registered_with_rendered_pixels_on_rounded_crops() {
        use std::sync::Arc;

        for depth in [8, 16] {
            for (width, height, edge) in [(1031, 5, 513), (5, 1031, 513), (49, 5, 14), (5, 49, 14)] {
                let rect = [7, 9, 7 + width, 9 + height];
                let mut doc = Document::new_depth((rect[2] + 11) as u32, (rect[3] + 13) as u32, depth).unwrap();
                let mut mask = Raster::new(width as u32, height as u32);
                for y in 0..height {
                    for x in 0..width {
                        let value = (1 + (x * 23 + y * 37) % 250) as u8;
                        doc.layers[0].pixels.set(x + rect[0], y + rect[1], [value, value, value, 255]);
                        mask.set(x, y, [value; 4]);
                    }
                }
                doc.selection = Some(rect);
                doc.selection_coverage = Some(crate::selection::Coverage {
                    mask, bounds: rect, origin: [rect[0], rect[1]], contours: vec![],
                });
                let original = serde_json::to_value(&doc).unwrap();
                let (scene, path) = crate::compositor::native::tests::capture_kind(&doc, true);
                for (mode, name) in [(SelectionView::Mask, "mask"), (SelectionView::Cutout, "cutout"), (SelectionView::Overlay, "overlay")] {
                    for edge in [edge, width.max(height) as u32] {
                        let (w, h, artwork, area) = doc.preview(Some(rect), edge, None, false).unwrap();
                        // Artwork and coverage carry the same value at each document coordinate.
                        let expected: Vec<_> = artwork.chunks_exact(4).flat_map(|pixel| {
                            project(pixel.try_into().unwrap(), pixel[0], mode)
                        }).collect();
                        let mut resident = artwork;
                        crate::selection::display::apply_region(&doc, w, h, &mut resident, area, name);
                        assert_eq!(resident, expected, "resident depth={depth}, rect={rect:?}, edge={edge}, mode={name}");

                        let mut prepared = Prepared::capture(
                            &scene, Some(rect), edge, None, false, Some(mode), &mut || Ok(()),
                        ).unwrap();
                        prepared.stream_grid(depth, edge);
                        let frame = Arc::new(prepared).render(&scene, &mut || Ok(())).unwrap();
                        assert_eq!([frame.key().width, frame.key().height], [w, h]);
                        assert_eq!(&*frame.copy_rgba8(&mut || Ok(())).unwrap(), &expected,
                            "native depth={depth}, rect={rect:?}, edge={edge}, mode={name}");
                    }
                }
                assert_eq!(serde_json::to_value(&doc).unwrap(), original);
                drop(scene);
                crate::compositor::native::tests::cleanup(&path);
            }
        }
    }
}

fn project(pixel: [u8; 4], byte: u8, mode: SelectionView) -> [u8; 4] {
    let value = (byte as f32 / 255.).clamp(0., 1.);
    match mode {
        SelectionView::Mask => [byte, byte, byte, 255],
        SelectionView::Overlay => crate::raster::blend(
            pixel,
            [233, 84, 32, ((1. - value) * 140.).round() as u8],
            1.,
            "normal",
        ),
        SelectionView::Cutout => [
            pixel[0],
            pixel[1],
            pixel[2],
            (pixel[3] as f32 * value).round() as u8,
        ],
    }
}
type Column<'a> = (Range<u32>, Option<PreparedSelection<'a>>);
struct Band<'a> {
    columns: Vec<Column<'a>>,
    top: i32,
    _index: Reservation,
}
impl<'a> Band<'a> {
    fn prepare(
        selection: &'a Selection<'a>,
        top: i32,
        width: u32,
        sample_x: &impl Fn(u32) -> i32,
        checks: &mut ReadChecks<'_>,
    ) -> Result<Self> {
        let origin = selection.origin()[0];
        let left = origin + (sample_x(0) - origin).div_euclid(512) * 512;
        let count = ((sample_x(width - 1) + 1 - left) as usize).div_ceil(512);
        let entry = std::mem::size_of::<Column<'a>>();
        let bytes = count * entry + 128;
        let index = selection
            .document()
            .budget()
            .reserve_bytes(bytes, 4 * 1024 * 1024)
            .map_err(|e| e.to_string())?;
        let mut columns = Vec::new();
        columns
            .try_reserve_exact(count)
            .map_err(|_| "Selection preview band allocation refused")?;
        if columns.capacity() * entry > index.bytes() {
            return Err("Selection preview band exceeds admission".into());
        }
        let mut start = 0;
        for column in 0..count {
            checks.pinned()?;
            let x = left + column as i32 * 512;
            let mut end = start;
            let mut high = width;
            while end < high {
                let middle = end + (high - end) / 2;
                if sample_x(middle) < x + 512 {
                    end = middle + 1;
                } else {
                    high = middle;
                }
            }
            if start == end {
                continue;
            }
            let bounds = [sample_x(start), top, sample_x(end - 1) + 1, top + 256];
            let view = if selection.may_cover(bounds) {
                let region = Region::new(bounds).map_err(|e| e.to_string())?;
                Some(checks.with_backing(|check| {
                    let checked = std::cell::RefCell::new(check);
                    selection.prepare(region, &|| (checked.borrow_mut())())
                        .map_err(|error| error.to_string())
                })?)
            } else {
                None
            };
            columns.push((start..end, view));
            start = end;
        }
        if start != width {
            return Err("Selection preview band does not cover the sampling grid".into());
        }
        Ok(Self {
            columns,
            top,
            _index: index,
        })
    }
}
