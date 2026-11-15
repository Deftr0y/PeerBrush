//! Native scalar refinement through explicit bounded source regions.
use crate::{
    large_document::{Error, Result, ScratchBytes, Store},
    preview::regions::{intersect, window},
    raster::{backed::Region, project16, surface::Surface, TILE},
};
use serde_json::Value;

pub(super) fn parameter(
    c: &Value,
    key: &str,
    min: f32,
    max: f32,
    default: f32,
) -> std::result::Result<f32, String> {
    let Some(v) = c.get(key) else {
        return Ok(default);
    };
    let v = v.as_f64().ok_or_else(|| format!("Invalid {key}"))?;
    if !v.is_finite() || v < min as f64 || v > max as f64 {
        return Err(format!("{key} must be between {min} and {max}"));
    }
    Ok(v as f32)
}
#[derive(Clone, Copy)]
struct Settings {
    smooth: usize,
    feather: usize,
    shift: i32,
    contrast: f32,
    edge: f32,
    radius: usize,
}
impl Settings {
    fn parse(c: &Value) -> std::result::Result<Self, String> {
        let p = |key, min, max, default| parameter(c, key, min, max, default);
        Ok(Self {
            smooth: p("smooth", 0., 32., 0.)?.round() as usize,
            feather: p("feather", 0., 64., 0.)?.round() as usize,
            shift: p("shift", -32., 32., 0.)?.round() as i32,
            contrast: p("contrast", 0., 100., 0.)? / 100.,
            edge: p("edge", 0., 100., 0.)? / 100.,
            radius: p("edge_radius", 1., 32., 8.)?.round() as usize,
        })
    }
    fn unchanged(self) -> bool {
        self.smooth == 0
            && self.feather == 0
            && self.shift == 0
            && self.contrast == 0.
            && self.edge == 0.
    }
}

fn tile_bounds(bounds: [i32; 4], reach: usize, [width, height]: [u32; 2]) -> [i32; 4] {
    let [l, t, r, b] = window(bounds, reach as i32, width, height).map(|v| v as u32);
    [
        l / TILE * TILE,
        t / TILE * TILE,
        (r.div_ceil(TILE) * TILE).min(width),
        (b.div_ceil(TILE) * TILE).min(height),
    ]
    .map(|v| v as i32)
}

pub(crate) struct SelectionPlan<'a> {
    input: &'a super::SelectionSource,
    size: [u32; 2],
    settings: Settings,
    source: Option<[i32; 4]>,
    base: [i32; 4],
    output: [i32; 4],
}

impl<'a> SelectionPlan<'a> {
    pub(crate) fn capture(
        input: &'a super::SelectionSource,
        size @ [width, height]: [u32; 2],
        c: &Value,
        check: &dyn Fn() -> std::result::Result<(), String>,
    ) -> std::result::Result<Self, String> {
        crate::raster::CanvasLimits::Native.check(width, height)?;
        let settings = Settings::parse(c)?;
        check()?;
        let source = match input {
            super::SelectionSource::All => {
                return Err("Select an area before refining its edge".into())
            }
            super::SelectionSource::Geometry { bounds, .. } => Some(*bounds),
            super::SelectionSource::Coverage(coverage) => {
                let mask = coverage.mask();
                let [ox, oy] = coverage.origin();
                let mut bounds = None;
                // Coverage bounds are a hint. Include every physical tile even
                // when an older mask has samples beyond that hint.
                for tile in mask.physical_tiles() {
                    check()?;
                    let (x, y) = tile.coordinate();
                    let rect = [
                        ox + (x * TILE) as i32,
                        oy + (y * TILE) as i32,
                        ox + ((x + 1) * TILE).min(mask.width()) as i32,
                        oy + ((y + 1) * TILE).min(mask.height()) as i32,
                    ];
                    let rect = intersect(rect, width, height);
                    if rect[0] < rect[2] && rect[1] < rect[3] {
                        bounds =
                            Some(bounds.map_or(rect, |old| {
                                crate::engine::reframe_scope::union(old, rect)
                            }));
                    }
                }
                bounds
            }
        }
        .map(|rect| intersect(rect, width, height))
        .filter(|rect| rect[0] < rect[2] && rect[1] < rect[3]);
        let base = source.map_or([0; 4], |bounds| {
            tile_bounds(
                bounds,
                settings.shift.unsigned_abs() as usize + settings.smooth + settings.feather,
                size,
            )
        });
        // Whole tiles preserve the floating-point filter's original summation origin.
        let output = if settings.edge > 0. && source.is_some() {
            tile_bounds(base, 2 * settings.radius, size)
        } else {
            base
        };
        Ok(Self {
            input,
            size,
            settings,
            source,
            base,
            output,
        })
    }

    pub(crate) fn guide_rect(&self, profiled: bool) -> Option<[i32; 4]> {
        if self.settings.edge == 0. || self.source.is_none() {
            return None;
        }
        let [width, height] = self.size;
        let mut rect = window(self.output, 2 * self.settings.radius as i32, width, height);
        if profiled {
            rect[0] = 0;
            rect[2] = width as i32;
        }
        Some(rect)
    }
}

fn zeroes<T: Default + Clone>(length: usize) -> Result<Vec<T>> {
    if length > 512 * 512 {
        return Err(Error::Budget);
    }
    let mut values = Vec::new();
    values
        .try_reserve_exact(length)
        .map_err(|_| Error::Budget)?;
    if values.capacity() > length {
        return Err(Error::Budget);
    }
    values.resize(length, T::default());
    Ok(values)
}

/// The source and output retain native precision. `guide` is the captured
/// rendered image projected to8bits exactly as the existing edge operator.
/// I/O, allocation and staging must run outside engine/UI locks.
pub fn prepare(
    input: &Surface,
    guide: Option<&Surface>,
    c: &Value,
    store: Store,
    check: &mut dyn FnMut() -> Result<()>,
) -> std::result::Result<Surface, String> {
    let s = Settings::parse(c)?;
    let frame = [0, 0, input.width() as i32, input.height() as i32];
    refine(
        input,
        guide.map(|guide| (guide, frame)),
        s,
        [frame; 2],
        store,
        check,
    )
}

fn refine(
    input: &Surface,
    guide: Option<(&Surface, [i32; 4])>,
    s: Settings,
    [base_bounds, output_bounds]: [[i32; 4]; 2],
    store: Store,
    check: &mut dyn FnMut() -> Result<()>,
) -> std::result::Result<Surface, String> {
    let pool = store.resident_budget();
    let disk = store
        .disk_budget()
        .ok_or("Refinement needs the shared scene disk admission")?;
    if !input.uses_budget(&pool)
        || !input.uses_disk_budget(&disk)
        || (s.edge > 0.
            && guide.is_some_and(|(g, rect)| {
                let needed = window(
                    output_bounds,
                    2 * s.radius as i32,
                    input.width(),
                    input.height(),
                );
                !g.uses_budget(&pool)
                    || !g.uses_disk_budget(&disk)
                    || rect != intersect(rect, input.width(), input.height())
                    || rect[0] > needed[0]
                    || rect[1] > needed[1]
                    || rect[2] < needed[2]
                    || rect[3] < needed[3]
                    || rect[2] - rect[0] != g.width() as i32
                    || rect[3] - rect[1] != g.height() as i32
            }))
    {
        return Err("Refinement sources differ from shared scene admission".into());
    }
    check().map_err(|e| e.to_string())?;
    if s.unchanged() {
        return Ok(input.clone());
    }
    if s.edge > 0. && guide.is_none() {
        return Err("Edge refinement needs an image matching the mask frame".into());
    }
    // All tile-local vectors coexist under this reservation and cannot escape.
    // The largest block is512²; guided refinement retains fewer than12f32
    // blocks384² plus native pins, charged separately by Surface preparation.
    let _scratch = pool
        .reserve_bytes(16 * 1024 * 1024, 16 * 1024 * 1024)
        .map_err(|e| e.to_string())?;
    let base = base(input, base_bounds, s, store.clone(), check).map_err(|e| e.to_string())?;
    let output = if s.edge > 0. {
        guided(&base, output_bounds, guide.unwrap(), s, store, check).map_err(|e| e.to_string())?
    } else {
        base
    };
    check().map_err(|e| e.to_string())?;
    Ok(output)
}

/// Prepare the ordinary selection command from geometry or raw soft coverage.
/// `None` retains the existing selection and its history on an unchanged result.
pub fn prepare_selection(
    input: &super::SelectionSource,
    width: u32,
    height: u32,
    guide: Option<&Surface>,
    c: &Value,
    store: Store,
    check: &dyn Fn() -> Result<()>,
) -> std::result::Result<Option<super::backed::CoverageSource>, String> {
    let plan = SelectionPlan::capture(input, [width, height], c, &|| {
        check().map_err(|e| e.to_string())
    })?;
    plan.prepare(
        guide.map(|guide| (guide, [0, 0, width as i32, height as i32])),
        store,
        check,
    )
}

impl SelectionPlan<'_> {
    pub(crate) fn prepare(
        &self,
        guide: Option<(&Surface, [i32; 4])>,
        store: Store,
        check: &dyn Fn() -> Result<()>,
    ) -> std::result::Result<Option<super::backed::CoverageSource>, String> {
        check().map_err(|e| e.to_string())?;
        if self.settings.unchanged() {
            return Ok(None);
        }
        let pool = store.resident_budget();
        if matches!(self.input, super::SelectionSource::Coverage(coverage) if !coverage.uses_budget(&pool))
        {
            return Err("Selection source uses a different payload ledger".into());
        }
        let Some(source_bounds) = self.source else {
            return Ok(None);
        };
        let source = stage_tiles(
            self.size,
            8,
            tile_bounds(source_bounds, 0, self.size),
            store.clone(),
            &mut || check(),
            |[left, top, right, bottom], bytes, check| {
                let view = self.input.prepare_region(
                    Region::new([left, top, right, bottom])?,
                    &pool,
                    check,
                )?;
                for y in top..bottom {
                    check()?;
                    for x in left..right {
                        let at = (((y - top) as u32 * TILE + (x - left) as u32) * 4) as usize;
                        let value = (view.value(x, y)? * 255.).round() as u8;
                        bytes[at..at + 4].fill(value);
                    }
                }
                Ok(())
            },
        )
        .map_err(|e| e.to_string())?;
        let output = refine(
            &source,
            guide,
            self.settings,
            [self.base, self.output],
            store.clone(),
            &mut || check(),
        )?;
        if same_pixels(&output, &source, self.output, &mut || check()).map_err(|e| e.to_string())? {
            return Ok(None);
        }
        let [width, height] = self.size;
        let [left, top, right, bottom] = self.output;
        (|| -> Result<_> {
            let mut contours = super::contours::Contours::new(width, height, &pool)?;
            let mut row = ScratchBytes::new(&pool, width as usize, width as usize)?;
            for y in 0..top {
                contours.row(y as u32, &row, &mut || check())?;
            }
            // The scalar output already owns complete, read-back native tiles.
            // Retain one band of source pins while deriving its display contours.
            let band_rows = (crate::raster::backed::MAX_REGION_PIXELS / (right - left) as usize) as i32;
            let mut start = top;
            while start < bottom {
                let tile_end = (start / TILE as i32 + 1) * TILE as i32;
                let end = (start + band_rows).min(tile_end).min(bottom);
                let view = output.prepare_region(Region::new([left, start, right, end])?, &mut || check())?;
                for y in start..end {
                    for (x, value) in (left..right).zip(&mut row[left as usize..right as usize]) {
                        *value = view.pixel(x, y)?[0];
                    }
                    contours.row(y as u32, &row, &mut || check())?;
                }
                start = end;
            }
            row.fill(0);
            for y in bottom..height as i32 {
                contours.row(y as u32, &row, &mut || check())?;
            }
            let (bounds, contours) = contours.finish(&mut || check())?;
            let coverage = super::backed::CoverageSource::from_frozen(output, bounds, [0, 0], contours)?;
            check()?;
            Ok(coverage)
        })()
        .map_err(|e| e.to_string())
        .map(Some)
    }
}

fn same_pixels(
    a: &Surface,
    b: &Surface,
    [left, top, right, bottom]: [i32; 4],
    check: &mut dyn FnMut() -> Result<()>,
) -> Result<bool> {
    check()?;
    if (a.width(), a.height(), a.depth()) != (b.width(), b.height(), b.depth()) {
        return Ok(false);
    }
    if a.key() == b.key() {
        return Ok(true);
    }
    let rows =
        (crate::raster::backed::MAX_REGION_PIXELS / (right - left) as usize).min(TILE as usize);
    for y0 in (top..bottom).step_by(rows) {
        let y1 = (y0 + rows as i32).min(bottom);
        let region = Region::new([left, y0, right, y1])?;
        let a = a.prepare_region(region, check)?;
        let b = b.prepare_region(region, check)?;
        for y in y0..y1 {
            check()?;
            for x in left..right {
                if a.pixel16(x, y)? != b.pixel16(x, y)? {
                    return Ok(false);
                }
            }
        }
    }
    check()?;
    Ok(true)
}

fn red(
    input: &Surface,
    area: [i32; 4],
    clamp: bool,
    check: &mut dyn FnMut() -> Result<()>,
) -> Result<Vec<u16>> {
    let [left, top, right, bottom] = area;
    let width = (right - left) as usize;
    let height = (bottom - top) as usize;
    let mut values = zeroes(width * height)?;
    let stripe = (128 * 1024 / width).min(256).max(1);
    for y0 in (0..height).step_by(stripe) {
        check()?;
        let y1 = (y0 + stripe).min(height);
        let bounds = if clamp {
            [
                left.clamp(0, input.width() as i32 - 1),
                (top + y0 as i32).clamp(0, input.height() as i32 - 1),
                (right - 1).clamp(0, input.width() as i32 - 1) + 1,
                (top + y1 as i32 - 1).clamp(0, input.height() as i32 - 1) + 1,
            ]
        } else {
            [left, top + y0 as i32, right, top + y1 as i32]
        };
        let view = input.prepare_region(Region::new(bounds)?, check)?;
        for y in y0..y1 {
            check()?;
            for x in 0..width {
                let (xx, yy) = if clamp {
                    (
                        (left + x as i32).clamp(0, input.width() as i32 - 1),
                        (top + y as i32).clamp(0, input.height() as i32 - 1),
                    )
                } else {
                    (left + x as i32, top + y as i32)
                };
                values[y * width + x] = view.pixel16(xx, yy)?[0];
            }
        }
    }
    Ok(values)
}

use super::morphology::line as morph;

fn shifted(
    input: &Surface,
    area: [i32; 4],
    shift: i32,
    check: &mut dyn FnMut() -> Result<()>,
) -> Result<Vec<u16>> {
    if shift == 0 {
        return red(input, area, false, check);
    }
    let r = shift.unsigned_abs() as usize;
    let [l, t, rr, b] = area;
    let w = (rr - l) as usize;
    let h = (b - t) as usize;
    let sw = w + 2 * r;
    let sh = h + 2 * r;
    let source = red(
        input,
        [l - r as i32, t - r as i32, rr + r as i32, b + r as i32],
        false,
        check,
    )?;
    let mut horizontal = zeroes(sw * sh)?;
    let mut queue = zeroes(sw.max(sh))?;
    for y in 0..sh {
        check()?;
        morph(
            &source[y * sw..(y + 1) * sw],
            r,
            shift > 0,
            &mut horizontal[y * sw..(y + 1) * sw],
            &mut queue,
        );
    }
    let mut column = zeroes(sh)?;
    let mut output = zeroes(sh)?;
    let mut values = zeroes(w * h)?;
    for x in 0..w {
        check()?;
        for y in 0..sh {
            column[y] = horizontal[y * sw + x + r];
        }
        morph(&column, r, shift > 0, &mut output, &mut queue);
        for y in 0..h {
            if l + x as i32 >= 0
                && t + y as i32 >= 0
                && l + (x as i32) < input.width() as i32
                && t + (y as i32) < input.height() as i32
            {
                values[y * w + x] = output[y + r];
            }
        }
    }
    Ok(values)
}

fn mean(
    input: &[f32],
    w: usize,
    h: usize,
    r: usize,
    check: &mut dyn FnMut() -> Result<()>,
) -> Result<Vec<f32>> {
    if r == 0 {
        let mut out = zeroes(input.len())?;
        out.copy_from_slice(input);
        return Ok(out);
    }
    let mut tmp = zeroes(input.len())?;
    let mut output = zeroes(input.len())?;
    let count = (2 * r + 1) as f32;
    for y in 0..h {
        check()?;
        let row = &input[y * w..(y + 1) * w];
        let mut sum = 0.;
        for k in -(r as isize)..=r as isize {
            sum += row[k.clamp(0, w as isize - 1) as usize];
        }
        for x in 0..w {
            tmp[y * w + x] = sum / count;
            sum += row[(x + r + 1).min(w - 1)] - row[x.saturating_sub(r)];
        }
    }
    for x in 0..w {
        check()?;
        let mut sum = 0.;
        for k in -(r as isize)..=r as isize {
            sum += tmp[k.clamp(0, h as isize - 1) as usize * w + x];
        }
        for y in 0..h {
            output[y * w + x] = sum / count;
            sum += tmp[(y + r + 1).min(h - 1) * w + x] - tmp[y.saturating_sub(r) * w + x];
        }
    }
    Ok(output)
}

fn write(bytes: &mut [u8], depth: u16, x: usize, y: usize, value: u16) {
    let at = (y * TILE as usize + x) * 4 * usize::from(depth / 8);
    if depth == 8 {
        bytes[at..at + 4].fill(project16(value));
    } else {
        for channel in bytes[at..at + 8].chunks_exact_mut(2) {
            channel.copy_from_slice(&value.to_be_bytes());
        }
    }
}

fn stage_tiles(
    [width, height]: [u32; 2],
    depth: u16,
    bounds: [i32; 4],
    store: Store,
    check: &mut dyn FnMut() -> Result<()>,
    mut render: impl FnMut([i32; 4], &mut [u8], &mut dyn FnMut() -> Result<()>) -> Result<()>,
) -> Result<Surface> {
    let mut writer = super::native_tiles::Writer::new(width, height, depth, store.clone())?;
    let columns = width.div_ceil(TILE);
    let total = columns * height.div_ceil(TILE);
    let n = (TILE * TILE * 4) as usize * usize::from(depth / 8);
    let mut bytes = ScratchBytes::new(&store.resident_budget(), n, n)?;
    let [left, top, right, bottom] = bounds.map(|v| v as u32);
    let mut next = 0;
    for y in top / TILE..bottom.div_ceil(TILE) {
        for x in left / TILE..right.div_ceil(TILE) {
            let index = y * columns + x;
            if index > next {
                writer.zero_tiles(index - next, check)?;
            }
            check()?;
            bytes.fill(0);
            let area = [
                x * TILE,
                y * TILE,
                ((x + 1) * TILE).min(width),
                ((y + 1) * TILE).min(height),
            ];
            render(area.map(|v| v as i32), &mut bytes, check)?;
            writer.tile(x, y, &bytes, check)?;
            next = index + 1;
        }
    }
    if next < total {
        writer.zero_tiles(total - next, check)?;
    }
    writer.finish(check)
}

fn base(
    input: &Surface,
    bounds: [i32; 4],
    s: Settings,
    store: Store,
    check: &mut dyn FnMut() -> Result<()>,
) -> Result<Surface> {
    stage_tiles(
        [input.width(), input.height()],
        input.depth(),
        bounds,
        store,
        check,
        |[left, top, right, bottom], bytes, check| {
            let tw = (right - left) as usize;
            let th = (bottom - top) as usize;
            let halo = s.smooth + s.feather;
            let w = tw + 2 * halo;
            let h = th + 2 * halo;
            let source = shifted(
                input,
                [
                    left - halo as i32,
                    top - halo as i32,
                    right + halo as i32,
                    bottom + halo as i32,
                ],
                s.shift,
                check,
            )?;
            let mut block = zeroes(w * h)?;
            for (v, &sample) in block.iter_mut().zip(&source) {
                *v = sample as f32 / 65535.;
            }
            drop(source);
            if s.smooth > 0 {
                block = mean(&block, w, h, s.smooth, check)?;
                for v in &mut block {
                    *v = ((*v - 0.5) * 4. + 0.5).clamp(0., 1.);
                }
            }
            if s.feather > 0 {
                block = mean(&block, w, h, s.feather, check)?;
            }
            for y in 0..th {
                check()?;
                for x in 0..tw {
                    let v = ((block[(y + halo) * w + x + halo] - 0.5) / (1. - s.contrast * 0.99)
                        + 0.5)
                        .clamp(0., 1.);
                    write(bytes, input.depth(), x, y, (v * 65535.).round() as u16);
                }
            }
            Ok(())
        },
    )
}

fn guided(
    input: &Surface,
    bounds: [i32; 4],
    (guide, [gx, gy, _, _]): (&Surface, [i32; 4]),
    s: Settings,
    store: Store,
    check: &mut dyn FnMut() -> Result<()>,
) -> Result<Surface> {
    stage_tiles(
        [input.width(), input.height()],
        input.depth(),
        bounds,
        store,
        check,
        |[left, top, right, bottom], bytes, check| {
            let tw = (right - left) as usize;
            let th = (bottom - top) as usize;
            let halo = 2 * s.radius;
            let w = tw + 2 * halo;
            let h = th + 2 * halo;
            let area = [
                left - halo as i32,
                top - halo as i32,
                right + halo as i32,
                bottom + halo as i32,
            ];
            let native = red(input, area, true, check)?;
            let mut source = zeroes(w * h)?;
            for (v, &sample) in source.iter_mut().zip(&native) {
                *v = sample as f32 / 65535.;
            }
            drop(native);
            let mut image = zeroes(w * h)?;
            let stripe = (128 * 1024 / w).min(256).max(1);
            for y0 in (0..h).step_by(stripe) {
                check()?;
                let y1 = (y0 + stripe).min(h);
                let bounds = [
                    area[0].clamp(0, input.width() as i32 - 1) - gx,
                    (area[1] + y0 as i32).clamp(0, input.height() as i32 - 1) - gy,
                    (area[2] - 1).clamp(0, input.width() as i32 - 1) + 1 - gx,
                    (area[1] + y1 as i32 - 1).clamp(0, input.height() as i32 - 1) + 1 - gy,
                ];
                let view = guide.prepare_region(Region::new(bounds)?, check)?;
                for y in y0..y1 {
                    check()?;
                    for x in 0..w {
                        let p = view.pixel(
                            (area[0] + x as i32).clamp(0, input.width() as i32 - 1) - gx,
                            (area[1] + y as i32).clamp(0, input.height() as i32 - 1) - gy,
                        )?;
                        image[y * w + x] =
                            (p[0] as f32 * 0.2126 + p[1] as f32 * 0.7152 + p[2] as f32 * 0.0722)
                                / 255.
                                * p[3] as f32
                                / 255.;
                    }
                }
            }
            let mi = mean(&image, w, h, s.radius, check)?;
            let mp = mean(&source, w, h, s.radius, check)?;
            let mut product = zeroes(w * h)?;
            for (v, &i) in product.iter_mut().zip(&image) {
                *v = i * i;
            }
            let ii = mean(&product, w, h, s.radius, check)?;
            for ((v, &i), &p) in product.iter_mut().zip(&image).zip(&source) {
                *v = i * p;
            }
            let ip = mean(&product, w, h, s.radius, check)?;
            drop(product);
            let mut a = zeroes(w * h)?;
            let mut b = zeroes(w * h)?;
            for i in 0..w * h {
                a[i] = (ip[i] - mi[i] * mp[i]) / ((ii[i] - mi[i] * mi[i]).max(0.) + 0.0001);
                b[i] = mp[i] - a[i] * mi[i];
            }
            let ma = mean(&a, w, h, s.radius, check)?;
            let mb = mean(&b, w, h, s.radius, check)?;
            for y in 0..th {
                check()?;
                for x in 0..tw {
                    let i = (y + halo) * w + x + halo;
                    let q = (ma[i] * image[i] + mb[i]).clamp(0., 1.);
                    let v = ((source[i] * (1. - s.edge) + q * s.edge) * 65535.)
                        .round()
                        .clamp(0., 65535.) as u16;
                    write(bytes, input.depth(), x, y, v);
                }
            }
            Ok(())
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        large_document::{DiskBudget, Limits, ResidentBudget},
        raster::Raster,
    };
    use serde_json::json;

    fn fixture(pool: &ResidentBudget) -> (Store, std::path::PathBuf) {
        let path = std::fs::canonicalize(std::env::temp_dir())
            .unwrap()
            .join(format!("peerbrush-native-refine-{}", crate::engine::id()));
        let store = Store::create_with_disk_budget(
            &path,
            pool.clone(),
            DiskBudget::new(256 * 1024 * 1024, 8).unwrap(),
            Limits {
                disk_bytes: 256 * 1024 * 1024,
                max_blocks: 4096,
                max_resident_blocks: 16,
                manifest_bytes: 1024 * 1024,
            },
        )
        .unwrap();
        (store, path)
    }

    #[test]
    fn bounded_refinement_matches_full_guide_with_offset_coverage_beyond_hint() {
        let size @ [width, height] = [1089, 1051];
        let origin = [773, 780];
        let pool = ResidentBudget::new(128 * 1024 * 1024).unwrap();
        let (store, path) = fixture(&pool);
        let mut full = Raster::new(width, height);
        let mut local = Raster::new(17, 15);
        for y in 0..15 {
            for x in 0..17 {
                let value = (35 + (x * 19 + y * 23) % 221) as u8;
                local.set(x, y, [value, 0, 0, 0]);
                full.set(x + origin[0], y + origin[1], [value; 4]);
            }
        }
        let mask = Surface::stage_raster(&local, store.clone(), &mut || Ok(())).unwrap();
        let input = super::super::SelectionSource::Coverage(
            super::super::backed::CoverageSource::new(
                mask,
                [780, 786, 782, 788],
                origin,
                &[],
                &pool,
            )
            .unwrap(),
        );
        let settings =
            json!({"shift":4,"smooth":3,"feather":8,"contrast":17,"edge":70,"edge_radius":5});
        let plan = SelectionPlan::capture(&input, size, &settings, &|| Ok(())).unwrap();
        let crop = plan.guide_rect(false).unwrap();
        let [left, top, right, bottom] = crop;
        assert_eq!(crop, [246, 246, width as i32, height as i32]);
        assert_eq!(plan.guide_rect(true).unwrap(), [0, top, right, bottom]);
        let mut full_guide = Raster::new(width, height);
        let mut crop_guide = Raster::new((right - left) as u32, (bottom - top) as u32);
        for y in 0..height as i32 {
            for x in 0..width as i32 {
                let color = [(x * 11) as u8, (y * 17) as u8, ((x + y) * 9) as u8, 255];
                full_guide.set(x, y, color);
                if x >= left && y >= top {
                    crop_guide.set(x - left, y - top, color);
                }
            }
        }
        let staged = |raster| Surface::stage_raster(raster, store.clone(), &mut || Ok(())).unwrap();
        let guide = staged(&crop_guide);
        let actual = plan
            .prepare(Some((&guide, crop)), store.clone(), &|| Ok(()))
            .unwrap()
            .unwrap();
        let expected = prepare(
            &staged(&full),
            Some(&staged(&full_guide)),
            &settings,
            store.clone(),
            &mut || Ok(()),
        )
        .unwrap();
        assert!(
            same_pixels(actual.mask(), &expected, [0, 0, right, bottom], &mut || Ok(
                ()
            ))
            .unwrap()
        );
        assert!(plan
            .prepare(
                Some((&guide, [left + 1, top, right, bottom])),
                store.clone(),
                &|| Ok(())
            )
            .is_err());
        drop((actual, expected, guide, input, store));
        assert_eq!(pool.used(), 0);
        std::fs::remove_dir_all(path).unwrap();
    }

    #[test]
    fn real_native8_and16_refinement_matches_legacy_operators_across_both_tile_edges() {
        for depth in [8, 16] {
            let (w, h) = (257u32, 259u32);
            let pool = ResidentBudget::new(96 * 1024 * 1024).unwrap();
            let (store, path) = fixture(&pool);
            let mut input = Raster::new_depth(w, h, depth);
            let mut guide = Raster::new(w, h);
            let mut pixels = vec![0; (w * h * 4) as usize];
            for y in 0..h {
                for x in 0..w {
                    let value = if (3..254).contains(&x) && (2..257).contains(&y) {
                        ((x * 1031 + y * 571 + 17) % 65536) as u16
                    } else {
                        0
                    };
                    input.set16(x as i32, y as i32, [value; 4]);
                    let color = [
                        (x * 17 + y * 3) as u8,
                        (x * 7 + y * 29) as u8,
                        (x * 19 + y * 5) as u8,
                        if x < 129 { 255 } else { 119 },
                    ];
                    guide.set(x as i32, y as i32, color);
                    let at = ((y * w + x) * 4) as usize;
                    pixels[at..at + 4].copy_from_slice(&color);
                }
            }
            let source = Surface::stage_raster(&input, store.clone(), &mut || Ok(())).unwrap();
            let guide_source =
                Surface::stage_raster(&guide, store.clone(), &mut || Ok(())).unwrap();
            for settings in [
                json!({}),
                json!({"shift":3}),
                json!({"shift":-4}),
                json!({"smooth":3,"feather":7,"contrast":40}),
                json!({"shift":-3,"smooth":2,"feather":3,"contrast":68,"edge":50,"edge_radius":3}),
                json!({"edge":100,"edge_radius":32}),
                json!({"shift":32,"smooth":32,"feather":64,"contrast":100}),
            ] {
                let expected =
                    crate::selection::refine::run(&input, Some(&pixels), &settings).unwrap();
                let output = prepare(
                    &source,
                    Some(&guide_source),
                    &settings,
                    store.clone(),
                    &mut || Ok(()),
                )
                .unwrap();
                assert_eq!(output.depth(), depth);
                for top in (0..h).step_by(32) {
                    let bounds = [0, top as i32, w as i32, (top + 32).min(h) as i32];
                    let view = output
                        .prepare_region(Region::new(bounds).unwrap(), &mut || Ok(()))
                        .unwrap();
                    let original = source
                        .prepare_region(Region::new(bounds).unwrap(), &mut || Ok(()))
                        .unwrap();
                    for y in top..(top + 32).min(h) {
                        for x in 0..w {
                            assert_eq!(
                                view.pixel16(x as i32, y as i32).unwrap(),
                                expected.get16(x as i32, y as i32),
                                "{depth} {settings} at {x},{y}"
                            );
                            assert_eq!(
                                original.pixel16(x as i32, y as i32).unwrap(),
                                input.get16(x as i32, y as i32)
                            );
                        }
                    }
                }
            }
            drop(source);
            drop(guide_source);
            drop(store);
            assert_eq!(pool.used(), 0);
            std::fs::remove_dir_all(path).unwrap();
        }
    }

    #[test]
    fn thin_frames_clamped_guides_and_cancellation_never_publish_a_partial_surface() {
        let pool = ResidentBudget::new(64 * 1024 * 1024).unwrap();
        let (store, path) = fixture(&pool);
        let mut input = Raster::new_depth(1, 3, 16);
        input.set16(0, 1, [12345; 4]);
        let mut guide = Raster::new(1, 3);
        guide.set(0, 0, [255, 29, 17, 193]);
        guide.set(0, 1, [13, 79, 201, 255]);
        let pixels: Vec<u8> = (0..3).flat_map(|y| guide.get(0, y)).collect();
        let source = Surface::stage_raster(&input, store.clone(), &mut || Ok(())).unwrap();
        let guide_source = Surface::stage_raster(&guide, store.clone(), &mut || Ok(())).unwrap();
        let settings = json!({"shift":-1,"feather":64,"edge":100,"edge_radius":32});
        let expected = crate::selection::refine::run(&input, Some(&pixels), &settings).unwrap();
        let output = prepare(
            &source,
            Some(&guide_source),
            &settings,
            store.clone(),
            &mut || Ok(()),
        )
        .unwrap();
        let view = output
            .prepare_region(Region::new([0, 0, 1, 3]).unwrap(), &mut || Ok(()))
            .unwrap();
        for y in 0..3 {
            assert_eq!(view.pixel16(0, y).unwrap(), expected.get16(0, y));
        }
        drop(view);
        drop(output);
        for stop in [0usize, 2, 20, 100] {
            let mut calls = 0;
            let result = prepare(
                &source,
                Some(&guide_source),
                &settings,
                store.clone(),
                &mut || {
                    calls += 1;
                    if calls > stop {
                        Err(Error::Cancelled)
                    } else {
                        Ok(())
                    }
                },
            );
            assert!(result.is_err());
        }
        assert!(prepare(
            &source,
            None,
            &json!({"edge":1}),
            store.clone(),
            &mut || Ok(())
        )
        .is_err());
        assert!(prepare(
            &source,
            None,
            &json!({"smooth":33}),
            store.clone(),
            &mut || Ok(())
        )
        .is_err());
        drop(source);
        drop(guide_source);
        drop(store);
        assert_eq!(pool.used(), 0);
        std::fs::remove_dir_all(path).unwrap();
    }

    #[test]
    fn selection_command_preparation_keeps_raw_coverage_and_geometry_priority_and_unchanged_state()
    {
        for soft in [false, true] {
            let pool = ResidentBudget::new(96 * 1024 * 1024).unwrap();
            let (store, path) = fixture(&pool);
            let mut doc = crate::engine::Document::new(33, 31).unwrap();
            let polygon = vec![[3., 3.], [29., 4.], [9., 27.]];
            let bounds = crate::selection::bounds(&polygon).unwrap();
            doc.selection = Some(bounds);
            doc.selection_polygon = Some(polygon.clone());
            let coverage = if soft {
                let mut mask = Raster::new(33, 31);
                mask.set(1, 1, [91; 4]);
                for y in 4..27 {
                    for x in 3..29 {
                        mask.set(x, y, [(x * 11 + y * 7) as u8; 4]);
                    }
                }
                let mut raw = crate::selection::from_mask(mask).unwrap();
                // A hint does not authorize dropping raw values outside it.
                raw.bounds = bounds;
                doc.selection_coverage = Some(raw.clone());
                Some(
                    super::super::backed::CoverageSource::stage_legacy(
                        &raw,
                        store.clone(),
                        &mut || Ok(()),
                    )
                    .unwrap(),
                )
            } else {
                None
            };
            let input = super::super::SelectionSource::from_metadata(
                Some(bounds),
                Some(std::sync::Arc::new(polygon)),
                coverage,
            )
            .unwrap();
            assert!(
                prepare_selection(&input, 33, 31, None, &json!({}), store.clone(), &|| Ok(()))
                    .unwrap()
                    .is_none()
            );
            let settings = json!({"shift":-1,"smooth":2,"feather":3,"contrast":17});
            crate::selection::refine::apply(&mut doc, &settings).unwrap();
            let expected = crate::selection::current(&doc);
            let candidate =
                prepare_selection(&input, 33, 31, None, &settings, store.clone(), &|| Ok(()))
                    .unwrap()
                    .unwrap();
            let reference = super::super::stage_rows(33, 31, store.clone(), &pool, &|| Ok(()), |y, row| {
                for (x, value) in row.iter_mut().enumerate() {
                    *value = expected.as_ref().map_or(0, |coverage| {
                        (coverage.value(x as i32, y as i32) * 255.).round() as u8
                    });
                }
                Ok(())
            }).unwrap();
            assert_eq!(candidate.origin(), reference.origin());
            assert_eq!(candidate.bounds(), reference.bounds());
            assert_eq!(candidate.contours(), reference.contours());
            let view = candidate
                .prepare_region(Region::new([0, 0, 33, 31]).unwrap(), &pool, &mut || Ok(()))
                .unwrap();
            for y in 0..31 {
                for x in 0..33 {
                    assert_eq!(
                        view.value(x, y).unwrap(),
                        expected.as_ref().map_or(0., |c| c.value(x, y)),
                        "{soft} at{x},{y}"
                    );
                }
            }
            drop(view);
            drop(reference);
            drop(candidate);
            drop(input);
            drop(store);
            assert_eq!(pool.used(), 0);
            std::fs::remove_dir_all(path).unwrap();
        }
    }
}
