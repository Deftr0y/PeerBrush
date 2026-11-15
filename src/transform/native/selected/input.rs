use crate::{
    engine::native::NativeDocument,
    large_document::{Error, Result},
    raster::{backed::Region, Pixel, Pixel16, Surface, SurfaceRegion},
    transform::native::Work,
};

pub(crate) struct Selection<'a> {
    document: &'a NativeDocument,
    pub bounds: [i32; 4],
    values: Values<'a>,
}
enum Values<'a> {
    Raw {
        mask: &'a Surface,
        origin: [i32; 2],
    },
    Geometry {
        frame: Option<[u32; 2]>,
        polygon: Option<&'a [[f32; 2]]>,
    },
}
impl<'a> Selection<'a> {
    pub(crate) fn new(document: &'a NativeDocument) -> std::result::Result<Self, String> {
        Self::build(document, false)
    }
    pub(crate) fn for_folder(document: &'a NativeDocument) -> std::result::Result<Self, String> {
        Self::build(document, true)
    }
    fn build(document: &'a NativeDocument, rasterized: bool) -> std::result::Result<Self, String> {
        let metadata = document.metadata();
        let bounds = metadata.selection().ok_or("Selection no longer exists")?;
        let values = if let Some(coverage) = metadata
            .selection_coverage()
            .filter(|m| m.bounds() == bounds)
        {
            let mask = document.source(coverage.mask().slot())?;
            if mask.depth() != 8
                || !mask.uses_budget(document.budget())
                || !mask.uses_disk_budget(document.disk_budget())
            {
                return Err("Selected transform coverage admission differs".into());
            }
            Values::Raw {
                mask,
                origin: coverage.origin(),
            }
        } else {
            // Folder dispatch uses selection::current (canvas-clipped scalar
            // coverage); ordinary layer dispatch keeps geometric polygons.
            Values::Geometry {
                frame: rasterized.then_some([metadata.width(), metadata.height()]),
                polygon: metadata
                    .selection_polygon()
                    .filter(|p| crate::selection::bounds(p).ok() == Some(bounds)),
            }
        };
        Ok(Self {
            document,
            bounds,
            values,
        })
    }
    pub(crate) fn document(&self) -> &'a NativeDocument {
        self.document
    }
    pub(crate) fn origin(&self) -> [i32; 2] {
        match self.values {
            Values::Raw { origin, .. } => origin,
            Values::Geometry { .. } => [0, 0],
        }
    }
    pub(crate) fn mask_frame(&self) -> [u32; 2] {
        match self.values {
            Values::Raw { mask, .. } => [mask.width(), mask.height()],
            Values::Geometry { frame, .. } => frame.unwrap_or([
                self.document.metadata().width(),
                self.document.metadata().height(),
            ]),
        }
    }
    pub(super) fn raw_mask(&self) -> Option<&'a Surface> {
        match self.values {
            Values::Raw { mask, .. } => Some(mask),
            Values::Geometry { .. } => None,
        }
    }
    pub(crate) fn rasterized(&self) -> bool {
        !matches!(self.values, Values::Geometry { frame: None, .. })
    }
    pub(crate) fn coverage_cost(&self) -> u64 {
        match self.values {
            Values::Geometry { polygon: Some(points), .. } => points.len() as u64,
            _ => 1,
        }
    }
    pub(crate) fn polygon(&self) -> Option<&'a [[f32; 2]]> {
        match self.values {
            Values::Geometry {
                frame: None,
                polygon,
            } => polygon,
            _ => None,
        }
    }
    pub(crate) fn may_cover(&self, [left, top, right, bottom]: [i32; 4]) -> bool {
        match self.values {
            Values::Raw { mask, origin: [x, y] } => {
                // Raw samples outside the selection bounds hint are still visible.
                mask.has_physical_tiles([left - x, top - y, right - x, bottom - y])
            }
            Values::Geometry { .. } => {
                let [l, t, r, b] = self.bounds;
                left < r && l < right && top < b && t < bottom
            }
        }
    }
    pub(crate) fn prepare(
        &self,
        region: Region,
        check: &dyn Fn() -> Result<()>,
    ) -> Result<Prepared<'_>> {
        check()?;
        let view = match self.values {
            Values::Raw { mask, origin } => {
                let b = region.bounds();
                let b = [
                    (b[0] - origin[0]).max(0),
                    (b[1] - origin[1]).max(0),
                    (b[2] - origin[0]).min(mask.width() as i32),
                    (b[3] - origin[1]).min(mask.height() as i32),
                ];
                if b[0] < b[2] && b[1] < b[3] {
                    Some(mask.prepare_region(Region::new(b)?, &mut || check())?)
                } else {
                    None
                }
            }
            Values::Geometry { .. } => None,
        };
        check()?;
        Ok(Prepared {
            source: self,
            region,
            view,
        })
    }
}
pub(crate) struct Prepared<'a> {
    source: &'a Selection<'a>,
    region: Region,
    view: Option<SurfaceRegion>,
}
impl Prepared<'_> {
    pub(crate) fn byte(&self, x: i32, y: i32, work: &mut Work) -> std::result::Result<u8, String> {
        let [l, t, r, b] = self.region.bounds();
        if x < l || x >= r || y < t || y >= b {
            return Err("Selected coverage sample exceeds prepared footprint".into());
        }
        match self.source.values {
            Values::Raw { mask, origin } => {
                work.claim(1)?;
                let (x, y) = (x - origin[0], y - origin[1]);
                if x < 0 || y < 0 || x >= mask.width() as i32 || y >= mask.height() as i32 {
                    return Ok(0);
                }
                self.view
                    .as_ref()
                    .ok_or("Missing selected coverage footprint")?
                    .pixel(x, y)
                    .map(|p| p[0])
                    .map_err(|e| e.to_string())
            }
            Values::Geometry { frame, polygon } => {
                work.claim(polygon.map_or(1, |p| p.len() as u64))?;
                let [l, t, r, b] = self.source.bounds;
                Ok(
                    if frame.is_none_or(|frame| {
                        x >= 0 && y >= 0 && x < frame[0] as i32 && y < frame[1] as i32
                    }) && x >= l
                        && x < r
                        && y >= t
                        && y < b
                        && polygon.is_none_or(|p| {
                            crate::selection::contains(p, x as f32 + 0.5, y as f32 + 0.5)
                        })
                    {
                        255
                    } else {
                        0
                    },
                )
            }
        }
    }
}

pub(super) struct Input<'a> {
    source: &'a Surface,
    pixels: SurfaceRegion,
    selection: Prepared<'a>,
    origin: [i32; 2],
    fill: Option<Pixel>,
}
impl<'a> Input<'a> {
    pub(super) fn prepare(
        source: &'a Surface,
        selection: &'a Selection<'a>,
        origin: [i32; 2],
        region: Region,
        fill: Option<Pixel>,
        check: &dyn Fn() -> Result<()>,
    ) -> Result<Self> {
        if !source.uses_budget(selection.document.budget())
            || !source.uses_disk_budget(selection.document.disk_budget())
        {
            return Err(Error::Invalid("Selected source uses another admission"));
        }
        let pixels = source.prepare_region(region, &mut || check())?;
        let [l, t, r, b] = region.bounds();
        let selection = selection.prepare(
            Region::new([l + origin[0], t + origin[1], r + origin[0], b + origin[1]])?,
            check,
        )?;
        Ok(Self {
            source,
            pixels,
            selection,
            origin,
            fill,
        })
    }
    fn pixel(&self, x: i32, y: i32) -> std::result::Result<Pixel16, String> {
        if x < 0 || y < 0 || x >= self.source.width() as i32 || y >= self.source.height() as i32 {
            return Ok([0; 4]);
        }
        if let Some(color) = self.fill {
            return Ok(color.map(|v| u16::from(v) * 257));
        }
        self.pixels.pixel16(x, y).map_err(|e| e.to_string())
    }
    fn factor(&self, x: i32, y: i32, work: &mut Work) -> std::result::Result<f32, String> {
        let (x, y) = (x + self.origin[0], y + self.origin[1]);
        let [l, t, r, b] = self.selection.source.bounds;
        // The virtual cut loops only the coverage bounds rectangle. Raw mask
        // pixels outside that hint remain available to scalar-mask resampling.
        if x < l || x >= r || y < t || y >= b {
            return Ok(0.);
        }
        Ok(f32::from(self.selection.byte(x, y, work)?) / 255.)
    }
    pub(super) fn cut(
        &self,
        x: i32,
        y: i32,
        work: &mut Work,
    ) -> std::result::Result<Pixel16, String> {
        if x < 0 || y < 0 || x >= self.source.width() as i32 || y >= self.source.height() as i32 {
            return Ok([0; 4]);
        }
        let t = self.factor(x, y, work)?;
        if t <= 0. {
            return Ok([0; 4]);
        }
        let mut pixel = self.pixel(x, y)?;
        pixel[3] = (f64::from(pixel[3]) * f64::from(t)).round() as u16;
        Ok(self.quantize(pixel))
    }
    pub(super) fn remainder(
        &self,
        x: i32,
        y: i32,
        work: &mut Work,
    ) -> std::result::Result<(Pixel16, bool), String> {
        if x < 0 || y < 0 || x >= self.source.width() as i32 || y >= self.source.height() as i32 {
            return Ok(([0; 4], false));
        }
        let before = self.pixel(x, y)?;
        let materialized =
            self.fill.is_some() && self.pixels.pixel16(x, y).map_err(|e| e.to_string())? != before;
        let t = self.factor(x, y, work)?;
        if t <= 0. {
            return Ok((before, materialized));
        }
        let mut pixel = before;
        pixel[3] = (f64::from(pixel[3]) * (1. - f64::from(t))).round() as u16;
        if pixel[3] == 0 {
            pixel = [0; 4];
        }
        let pixel = self.quantize(pixel);
        Ok((pixel, materialized || pixel != before))
    }
    fn quantize(&self, pixel: Pixel16) -> Pixel16 {
        if self.source.depth() == 8 {
            pixel.map(|v| u16::from(crate::raster::project16(v)) * 257)
        } else {
            pixel
        }
    }
}
