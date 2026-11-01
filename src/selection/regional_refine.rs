//! Native scalar refinement through explicit bounded source regions.
use crate::{
    large_document::{Error, Result, ScratchBytes, Store},
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
    let pool = store.resident_budget();
    let disk = store
        .disk_budget()
        .ok_or("Refinement needs the shared scene disk admission")?;
    if !input.uses_budget(&pool)
        || !input.uses_disk_budget(&disk)
        || (s.edge > 0.
            && guide.is_some_and(|g| {
                !g.uses_budget(&pool)
                    || !g.uses_disk_budget(&disk)
                    || (g.width(), g.height()) != (input.width(), input.height())
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
    let base = base(input, s, store.clone(), check).map_err(|e| e.to_string())?;
    let output = if s.edge > 0. {
        guided(&base, guide.unwrap(), s, store, check).map_err(|e| e.to_string())?
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
    if matches!(input, super::SelectionSource::All) {
        return Err("Select an area before refining its edge".into());
    }
    let settings = Settings::parse(c)?;
    check().map_err(|e| e.to_string())?;
    if settings.unchanged() {
        return Ok(None);
    }
    let pool = store.resident_budget();
    let mut writer = super::native_tiles::Writer::new(width, height, 8, store.clone())
        .map_err(|e| e.to_string())?;
    let tile_bytes = (TILE * TILE * 4) as usize;
    let mut bytes = ScratchBytes::new(&pool, tile_bytes, tile_bytes).map_err(|e| e.to_string())?;
    for top in (0..height).step_by(TILE as usize) {
        for left in (0..width).step_by(TILE as usize) {
            check().map_err(|e| e.to_string())?;
            bytes.fill(0);
            let right = (left + TILE).min(width);
            let bottom = (top + TILE).min(height);
            let view = input
                .prepare_region(
                    Region::new([left as i32, top as i32, right as i32, bottom as i32])
                        .map_err(|e| e.to_string())?,
                    &pool,
                    &mut || check(),
                )
                .map_err(|e| e.to_string())?;
            for y in top..bottom {
                check().map_err(|e| e.to_string())?;
                for x in left..right {
                    let at = (((y - top) * TILE + x - left) * 4) as usize;
                    let value = (view.value(x as i32, y as i32).map_err(|e| e.to_string())? * 255.)
                        .round() as u8;
                    bytes[at..at + 4].fill(value);
                }
            }
            writer
                .tile(left / TILE, top / TILE, &bytes, &mut || check())
                .map_err(|e| e.to_string())?;
        }
    }
    drop(bytes);
    let source = writer.finish(&mut || check()).map_err(|e| e.to_string())?;
    let output = prepare(&source, guide, c, store.clone(), &mut || check())?;
    if output.key() == source.key() {
        return Ok(None);
    }
    let mut writer = super::coverage_writer::Writer::new(width, height, store, &pool)
        .map_err(|e| e.to_string())?;
    let allocate = |bytes| ScratchBytes::new(&pool, bytes, bytes).map_err(|e| e.to_string());
    let mut ring = allocate(width as usize)?;
    let mut sums = allocate(width as usize * 4)?;
    let mut input = allocate(width as usize)?;
    let mut row = allocate(width as usize)?;
    let complete =
        super::rows::Feather::new(width, height, 0, &mut ring, &mut sums, &mut input, &mut row)
            .map_err(str::to_owned)?
            .run(
                || check().map_err(|e| e.to_string()),
                |y, row| {
                    let view = output
                        .prepare_region(
                            Region::new([0, y as i32, width as i32, y as i32 + 1])
                                .map_err(|e| e.to_string())?,
                            &mut || check(),
                        )
                        .map_err(|e| e.to_string())?;
                    for (x, value) in row.iter_mut().enumerate() {
                        *value = view.pixel(x as i32, y as i32).map_err(|e| e.to_string())?[0];
                    }
                    Ok(())
                },
                |y, row| {
                    writer
                        .row(y, row, &mut || check())
                        .map_err(|e| e.to_string())
                },
            )?;
    let candidate = writer
        .finish(&complete, &mut || check())
        .map_err(|e| e.to_string())?;
    check().map_err(|e| e.to_string())?;
    Ok(Some(candidate))
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

fn morph(input: &[u16], r: usize, expand: bool, output: &mut [u16], queue: &mut [usize]) {
    let (mut head, mut tail, mut end) = (0, 0, 0);
    for (i, v) in output.iter_mut().enumerate() {
        while end < (i + r + 1).min(input.len()) {
            while tail > head
                && if expand {
                    input[queue[tail - 1]] <= input[end]
                } else {
                    input[queue[tail - 1]] >= input[end]
                }
            {
                tail -= 1;
            }
            queue[tail] = end;
            tail += 1;
            end += 1;
        }
        while tail > head && queue[head] < i.saturating_sub(r) {
            head += 1;
        }
        *v = if !expand && (i < r || i + r >= input.len()) {
            0
        } else {
            input[queue[head]]
        };
    }
}

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

fn base(
    input: &Surface,
    s: Settings,
    store: Store,
    check: &mut dyn FnMut() -> Result<()>,
) -> Result<Surface> {
    let mut writer = super::native_tiles::Writer::new(
        input.width(),
        input.height(),
        input.depth(),
        store.clone(),
    )?;
    let n = (TILE * TILE * 4) as usize * usize::from(input.depth() / 8);
    let mut bytes = ScratchBytes::new(&store.resident_budget(), n, n)?;
    for top in (0..input.height()).step_by(TILE as usize) {
        for left in (0..input.width()).step_by(TILE as usize) {
            check()?;
            bytes.fill(0);
            let tw = (input.width() - left).min(TILE) as usize;
            let th = (input.height() - top).min(TILE) as usize;
            let halo = s.smooth + s.feather;
            let w = tw + 2 * halo;
            let h = th + 2 * halo;
            let source = shifted(
                input,
                [
                    left as i32 - halo as i32,
                    top as i32 - halo as i32,
                    left as i32 + tw as i32 + halo as i32,
                    top as i32 + th as i32 + halo as i32,
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
                    write(&mut bytes, input.depth(), x, y, (v * 65535.).round() as u16);
                }
            }
            writer.tile(left / TILE, top / TILE, &bytes, check)?;
        }
    }
    writer.finish(check)
}

fn guided(
    input: &Surface,
    guide: &Surface,
    s: Settings,
    store: Store,
    check: &mut dyn FnMut() -> Result<()>,
) -> Result<Surface> {
    let mut writer = super::native_tiles::Writer::new(
        input.width(),
        input.height(),
        input.depth(),
        store.clone(),
    )?;
    let n = (TILE * TILE * 4) as usize * usize::from(input.depth() / 8);
    let mut bytes = ScratchBytes::new(&store.resident_budget(), n, n)?;
    for top in (0..input.height()).step_by(TILE as usize) {
        for left in (0..input.width()).step_by(TILE as usize) {
            check()?;
            bytes.fill(0);
            let tw = (input.width() - left).min(TILE) as usize;
            let th = (input.height() - top).min(TILE) as usize;
            let halo = 2 * s.radius;
            let w = tw + 2 * halo;
            let h = th + 2 * halo;
            let area = [
                left as i32 - halo as i32,
                top as i32 - halo as i32,
                left as i32 + tw as i32 + halo as i32,
                top as i32 + th as i32 + halo as i32,
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
                    area[0].clamp(0, input.width() as i32 - 1),
                    (area[1] + y0 as i32).clamp(0, input.height() as i32 - 1),
                    (area[2] - 1).clamp(0, input.width() as i32 - 1) + 1,
                    (area[1] + y1 as i32 - 1).clamp(0, input.height() as i32 - 1) + 1,
                ];
                let view = guide.prepare_region(Region::new(bounds)?, check)?;
                for y in y0..y1 {
                    check()?;
                    for x in 0..w {
                        let p = view.pixel(
                            (area[0] + x as i32).clamp(0, input.width() as i32 - 1),
                            (area[1] + y as i32).clamp(0, input.height() as i32 - 1),
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
                    write(&mut bytes, input.depth(), x, y, v);
                }
            }
            writer.tile(left / TILE, top / TILE, &bytes, check)?;
        }
    }
    writer.finish(check)
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
            drop(candidate);
            drop(input);
            drop(store);
            assert_eq!(pool.used(), 0);
            std::fs::remove_dir_all(path).unwrap();
        }
    }
}
