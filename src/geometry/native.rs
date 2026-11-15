//! Bounded image geometry sampling at original channel depth.
use crate::{
    large_document::{Error, ScratchBytes, Store, TileSpec},
    raster::{backed::{Region, MAX_REGION_PIXELS}, surface::TilePatch, Surface, TILE},
    selection::backed::CoverageSource,
};
type Result<T> = std::result::Result<T, String>;

type Index = fn(u32, u32, u32) -> i32;

fn color_index(at: u32, source: u32, target: u32) -> i32 {
    super::input::coordinate(at, source, target).floor() as i32
}

fn coverage_index(at: u32, source: u32, target: u32) -> i32 {
    super::input::coverage_coordinate(at, source, target).floor() as i32
}

fn source_window(source: &Surface, size: [u32; 2], area: [u32; 4], index: Index) -> [i32; 4] {
    let x = |at| index(at, source.width(), size[0]);
    let y = |at| index(at, source.height(), size[1]);
    [x(area[0]), y(area[1]), x(area[2] - 1) + 2, y(area[3] - 1) + 2]
}

fn windows(source: &Surface, size: [u32; 2], index: Index) -> impl Iterator<Item = ([i32; 4], [i32; 4])> + '_ {
    (0..size[0].div_ceil(TILE)).flat_map(move |tx| {
        (0..size[1].div_ceil(TILE)).map(move |ty| {
            let area = [tx * TILE, ty * TILE, ((tx + 1) * TILE).min(size[0]), ((ty + 1) * TILE).min(size[1])];
            (area.map(|v| v as i32), source_window(source, size, area, index))
        })
    })
}

pub(crate) fn resize_work(source: &Surface, size: [u32; 2], check: &dyn Fn() -> Result<()>) -> Result<u64> {
    if [source.width(), source.height()] == size {
        return Ok(0);
    }
    sample_work(source, size, color_index, check)
}

pub(crate) fn coverage_resize_work(source: &Surface, size: [u32; 2], check: &dyn Fn() -> Result<()>) -> Result<u64> {
    sample_work(source, size, coverage_index, check)
}

fn sample_work(source: &Surface, size: [u32; 2], index: Index, check: &dyn Fn() -> Result<()>) -> Result<u64> {
    if source.is_known_zero() {
        return Ok(0);
    }
    let mut samples = 0;
    for (area, reads) in windows(source, size, index) {
        check()?;
        if source.has_physical_tiles(reads) {
            samples += (area[2] - area[0]) as u64 * (area[3] - area[1]) as u64;
        }
    }
    Ok(samples)
}

pub(crate) fn resized(
    source: &Surface,
    size: [u32; 2],
    store: &Store,
    check: &dyn Fn() -> crate::large_document::Result<()>,
    pinned: &dyn Fn() -> Result<()>,
) -> Result<Surface> {
    if [source.width(), source.height()] == size {
        return Ok(source.clone());
    }
    if source.is_known_zero() {
        return Surface::zero(size[0], size[1], source.depth()).map_err(|e| e.to_string());
    }
    let pool = store.resident_budget();
    let disk = store.disk_budget().ok_or_else(|| "Resize needs its disk pool".to_owned())?;
    let count = size[0].div_ceil(TILE) as usize * size[1].div_ceil(TILE) as usize;
    let bytes = count * std::mem::size_of::<TilePatch>() + 128;
    let _charge = pool.reserve_bytes(bytes, bytes).map_err(|e| e.to_string())?;
    let mut patches = Vec::new();
    patches.try_reserve_exact(count).map_err(|_| "Resize tile index allocation refused")?;
    if patches.capacity() > count { return Err("Resize tile index exceeds admission".into()); }
    let spec = TileSpec::new(TILE as u16, TILE as u16, source.depth()).map_err(|e| e.to_string())?;
    let native16 = source.depth() == 16;
    let stride = usize::from(source.depth() / 8) * 4;
    for (area, reads) in windows(source, size, color_index) {
        check().map_err(|e| e.to_string())?;
        if !source.has_physical_tiles(reads) { continue; }
        let mut tile = ScratchBytes::new(&pool, spec.bytes(), spec.bytes()).map_err(|e| e.to_string())?;
        let mut source_x = [0.; TILE as usize];
        let source_x = &mut source_x[..(area[2] - area[0]) as usize];
        for (sx, x) in source_x.iter_mut().zip(area[0]..area[2]) {
            *sx = super::input::coordinate(x as u32, source.width(), size[0]);
        }
        let mut source_y = [0.; TILE as usize];
        let source_y = &mut source_y[..(area[3] - area[1]) as usize];
        for (sy, y) in source_y.iter_mut().zip(area[1]..area[3]) {
            *sy = super::input::coordinate(y as u32, source.height(), size[1]);
        }
        let max_rows = (MAX_REGION_PIXELS / (reads[2] - reads[0]) as usize) as i32;
        let mut start = 0;
        while start < source_y.len() {
            let top = source_y[start].floor() as i32;
            let mut rows = source_y[start..]
                .partition_point(|y| y.floor() as i32 + 2 <= top + max_rows)
                .max(1);
            let view = loop {
                let bottom = source_y[start + rows - 1].floor() as i32 + 2;
                let region = Region::new([reads[0], top, reads[2], bottom]).map_err(|e| e.to_string())?;
                match source.prepare_region(region, &mut || check()) {
                    Ok(view) => break view,
                    Err(Error::Budget) if rows > 1 => rows /= 2,
                    Err(error) => return Err(error.to_string()),
                }
            };
            for (row, &sy) in source_y[start..start + rows].iter().enumerate() {
                pinned()?;
                for (column, &sx) in source_x.iter().enumerate() {
                    let at = ((start + row) * TILE as usize + column) * stride;
                    let output = &mut tile[at..at + stride];
                    if native16 {
                        let value = crate::raster::sampling::sample16(sx, sy, |x, y| view.pixel16(x, y))
                            .map_err(|e: Error| e.to_string())?;
                        for (pair, value) in output.chunks_exact_mut(2).zip(value) {
                            pair.copy_from_slice(&value.to_be_bytes());
                        }
                    } else {
                        let value = crate::raster::sampling::sample(sx, sy, |x, y| view.pixel(x, y))
                            .map_err(|e: Error| e.to_string())?;
                        output.copy_from_slice(&value);
                    }
                }
            }
            start += rows;
        }
        if tile.iter().any(|v| *v != 0) {
            patches.push(TilePatch::stored(
                (area[0] as u32 / TILE, area[1] as u32 / TILE), store.clone(),
                store.put(spec, &tile, &mut || check()).map_err(|e| e.to_string())?,
            ));
        }
    }
    check().map_err(|e| e.to_string())?;
    Surface::zero(size[0], size[1], source.depth())
        .and_then(|surface| surface.with_tile_patches(&patches, &pool, &disk))
        .map_err(|e| e.to_string())
}

pub(crate) fn coverage(
    source: &Surface,
    size: [u32; 2],
    store: Store,
    check: &dyn Fn() -> crate::large_document::Result<()>,
) -> Result<CoverageSource> {
    if source.depth() != 8 {
        return Err("Liquify coverage requires native8 scalar sources".into());
    }
    let pool = store.resident_budget();
    if source.is_known_zero() {
        check().map_err(|e| e.to_string())?;
        return Surface::zero(size[0], size[1], 8)
            .and_then(|mask| CoverageSource::new(mask, [0; 4], [0, 0], &[], &pool))
            .map_err(|e| e.to_string());
    }
    crate::selection::stage_rows(size[0], size[1], store, &pool, check, |y, row| {
        row.fill(0);
        for (column, samples) in row.chunks_mut(TILE as usize).enumerate() {
            let left = column as u32 * TILE;
            let reads = source_window(source, size, [left, y, left + samples.len() as u32, y + 1], coverage_index);
            if !source.has_physical_tiles(reads) {
                continue;
            }
            let view = source
                .prepare_region(Region::new(reads).map_err(|e| e.to_string())?, &mut || check())
                .map_err(|e| e.to_string())?;
            for (x, value) in samples.iter_mut().enumerate() {
                *value = super::input::coverage(
                    left + x as u32, y, [source.width(), source.height()], size,
                    |x, y| view.pixel(x, y).map(|p| p[0]),
                ).map_err(|e| e.to_string())?;
            }
        }
        Ok(())
    })
}
