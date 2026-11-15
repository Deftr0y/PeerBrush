//! Bounded image scaling and composition; source pixels never become a resident frame.
use crate::{
    large_document::{Error, Result, ScratchBytes, Store, TileSpec},
    placement::input::coordinate,
    raster::{backed::Region, surface::TilePatch, Surface, TILE},
};

pub(super) struct Background<'a> {
    pub pixels: &'a Surface,
    /// Old origin relative to the output frame.
    pub shift: [i32; 2],
    pub fill: Option<[u8; 4]>,
}
pub(super) struct Image<'a> {
    pub pixels: &'a Surface,
    /// Destination rectangle relative to the output frame.
    pub area: [i32; 4],
    pub replace: bool,
}

pub(super) fn render(
    size: [u32; 2],
    depth: u16,
    background: Option<Background<'_>>,
    image: Option<Image<'_>>,
    store: &Store,
    check: &mut dyn FnMut() -> Result<()>,
) -> Result<(Surface, bool)> {
    let pool = store.resident_budget();
    let disk = store
        .disk_budget()
        .ok_or(Error::Invalid("Image output needs its disk pool"))?;
    let count = size[0].div_ceil(TILE) as usize * size[1].div_ceil(TILE) as usize;
    let bytes = count * std::mem::size_of::<TilePatch>() + 128;
    let _charge = pool.reserve_bytes(bytes, bytes)?;
    let mut patches = Vec::new();
    patches
        .try_reserve_exact(count)
        .map_err(|_| Error::Budget)?;
    if patches.capacity() > count {
        return Err(Error::Budget);
    }
    let spec = TileSpec::new(TILE as u16, TILE as u16, depth)?;
    let stride = usize::from(depth / 8) * 4;
    let same_frame = background.as_ref().is_some_and(|old| {
        old.shift == [0, 0]
            && [old.pixels.width(), old.pixels.height()] == size
            && old.fill.is_none()
    });
    let reuse = background
        .as_ref()
        .filter(|old| same_frame && old.pixels.depth() == depth)
        .map(|old| old.pixels);
    let mut changed = !same_frame;
    for tx in 0..size[0].div_ceil(TILE) {
        for ty in 0..size[1].div_ceil(TILE) {
            check()?;
            let left = (tx * TILE) as i32;
            let top = (ty * TILE) as i32;
            let right = ((tx + 1) * TILE).min(size[0]) as i32;
            let bottom = ((ty + 1) * TILE).min(size[1]) as i32;
            let draws = image.as_ref().is_some_and(|image| {
                image.area[0] < right && image.area[1] < bottom
                    && image.area[2] > left && image.area[3] > top
            });
            if reuse.is_some() && !draws {
                continue;
            }
            let old = background.as_ref().filter(|old| {
                let [dx, dy] = old.shift;
                let rect = [left - dx, top - dy, right - dx, bottom - dy];
                old.fill.map_or_else(|| old.pixels.has_physical_tiles(rect), |color| {
                    color.iter().any(|v| *v != 0) && rect[0] < old.pixels.width() as i32
                        && rect[1] < old.pixels.height() as i32 && rect[2] > 0 && rect[3] > 0
                })
            });
            if old.is_none() && !draws {
                continue;
            }
            if let Some(old) = old.filter(|old| !draws && old.fill.is_none() && old.pixels.depth() == depth) {
                let (x, y) = (left - old.shift[0], top - old.shift[1]);
                let edge = TILE as i32;
                // Only full logical tiles can move by reference. Source edge
                // padding must be rebuilt before a larger frame can expose it.
                if right - left == edge && bottom - top == edge && x >= 0 && y >= 0
                    && x % edge == 0 && y % edge == 0
                    && x + edge <= old.pixels.width() as i32 && y + edge <= old.pixels.height() as i32
                {
                    let coordinate = (x as u32 / TILE, y as u32 / TILE);
                    let physical = old.pixels.physical_tile(coordinate).ok_or(Error::Changed)?;
                    let pinned = old.pixels.prepare_tile(coordinate, check)?;
                    let bytes = pinned.bytes().ok_or(Error::Changed)?;
                    let mut nonzero = false;
                    for chunk in bytes.chunks(64 * 1024) {
                        check()?;
                        nonzero |= chunk.iter().any(|v| *v != 0);
                    }
                    if nonzero {
                        patches.push(TilePatch::stored((tx, ty), physical.owner().clone(), physical.reference()));
                    }
                    continue;
                }
            }
            let mut tile_changed = false;
            let mut tile = ScratchBytes::new(&pool, spec.bytes(), spec.bytes())?;
            if let Some(old) = old {
                if same_frame {
                    // Preserve existing physical edge padding as well as visible samples.
                    let input = old.pixels.prepare_tile((tx, ty), check)?;
                    if let Some(bytes) = input.bytes() {
                        if old.pixels.depth() == depth {
                            tile.copy_from_slice(bytes);
                        } else if depth == 16 {
                            for (v, out) in bytes.iter().zip(tile.chunks_exact_mut(2)) {
                                out.copy_from_slice(&(u16::from(*v) * 257).to_be_bytes());
                            }
                        } else {
                            return Err(Error::Invalid("Placement cannot reduce source precision"));
                        }
                    }
                } else {
                    let [dx, dy] = old.shift;
                    let region = old.pixels.prepare_region(
                        Region::new([left - dx, top - dy, right - dx, bottom - dy])?,
                        check,
                    )?;
                    for y in top..bottom {
                        check()?;
                        if old.fill.is_none() && old.pixels.depth() == depth {
                            let at = (y - top) as usize * TILE as usize * stride;
                            region.copy_row(y - dy, &mut tile[at..at + (right - left) as usize * stride])?;
                            continue;
                        }
                        for x in left..right {
                            let pixel = if let Some(color) = old.fill {
                                if x - dx >= 0
                                    && y - dy >= 0
                                    && x - dx < old.pixels.width() as i32
                                    && y - dy < old.pixels.height() as i32
                                {
                                    color.map(|v| u16::from(v) * 257)
                                } else {
                                    [0; 4]
                                }
                            } else {
                                region.pixel16(x - dx, y - dy)?
                            };
                            write(
                                &mut tile,
                                ((y - top) as usize * TILE as usize + (x - left) as usize) * stride,
                                depth,
                                pixel,
                            );
                        }
                    }
                }
            }
            if let Some(image) = &image {
                let area = image.area;
                let x0 = left.max(area[0]);
                let x1 = right.min(area[2]);
                let y0 = top.max(area[1]);
                let y1 = bottom.min(area[3]);
                if x0 < x1 && y0 < y1 {
                    let sw = image.pixels.width();
                    let sh = image.pixels.height();
                    let dw = (area[2] - area[0]) as u32;
                    let dh = (area[3] - area[1]) as u32;
                    let sx = |x: i32| coordinate((x - area[0]) as u32, sw, dw);
                    let sy = |y: i32| coordinate((y - area[1]) as u32, sh, dh);
                    let natural = if sw == dw && sh == dh {
                        Some(image.pixels.prepare_region(
                            Region::new([x0 - area[0], y0 - area[1], x1 - area[0], y1 - area[1]])?,
                            check,
                        )?)
                    } else {
                        None
                    };
                    let mut row = if natural.is_some() && image.replace && image.pixels.depth() == depth {
                        let bytes = (x1 - x0) as usize * stride;
                        Some(ScratchBytes::new(&pool, bytes, bytes)?)
                    } else {
                        None
                    };
                    for y in y0..y1 {
                        check()?;
                        let fy = sy(y);
                        let scaled;
                        let region = if let Some(region) = &natural {
                            region
                        } else {
                            // A two-source-row window also bounds extreme downsampling.
                            scaled = image.pixels.prepare_region(
                                Region::new([
                                    sx(x0).floor() as i32,
                                    fy.floor() as i32,
                                    sx(x1 - 1).floor() as i32 + 2,
                                    fy.floor() as i32 + 2,
                                ])?,
                                check,
                            )?;
                            &scaled
                        };
                        if let Some(row) = &mut row {
                            region.copy_row(y - area[1], row)?;
                            let at = ((y - top) as usize * TILE as usize + (x0 - left) as usize) * stride;
                            let output = &mut tile[at..at + row.len()];
                            tile_changed |= output != &row[..];
                            output.copy_from_slice(row);
                            continue;
                        }
                        for x in x0..x1 {
                            let at =
                                ((y - top) as usize * TILE as usize + (x - left) as usize) * stride;
                            if depth == 16 {
                                let old = std::array::from_fn(|c| {
                                    u16::from_be_bytes([tile[at + c * 2], tile[at + c * 2 + 1]])
                                });
                                let pixel =
                                    crate::raster::sampling::sample16(sx(x), fy, |x, y| {
                                        region.pixel16(x, y)
                                    })?;
                                let pixel = if image.replace {
                                    pixel
                                } else {
                                    crate::raster::blend16(old, pixel, 1., "normal")
                                };
                                tile_changed |= old != pixel;
                                write(&mut tile, at, depth, pixel);
                            } else {
                                let old = std::array::from_fn(|c| tile[at + c]);
                                let pixel = crate::raster::sampling::sample(sx(x), fy, |x, y| {
                                    region.pixel(x, y)
                                })?;
                                let pixel = if image.replace {
                                    pixel
                                } else {
                                    crate::raster::blend(old, pixel, 1., "normal")
                                };
                                tile_changed |= old != pixel;
                                tile[at..at + 4].copy_from_slice(&pixel);
                            }
                        }
                    }
                }
            }
            changed |= tile_changed;
            if reuse.is_some() && !tile_changed {
                continue;
            }
            if tile.iter().any(|v| *v != 0) {
                patches.push(TilePatch::stored(
                    (tx, ty),
                    store.clone(),
                    store.put(spec, &tile, check)?,
                ));
            } else if reuse.is_some() {
                patches.push(TilePatch::zero((tx, ty), spec, &tile.freeze(), &pool)?);
            }
        }
    }
    check()?;
    let source = if let Some(source) = reuse {
        if patches.is_empty() {
            source.clone()
        } else {
            source.with_tile_patches(&patches, &pool, &disk)?
        }
    } else {
        Surface::zero(size[0], size[1], depth)?.with_tile_patches(&patches, &pool, &disk)?
    };
    Ok((source, changed))
}
fn write(tile: &mut [u8], at: usize, depth: u16, pixel: [u16; 4]) {
    if depth == 16 {
        for (c, v) in pixel.into_iter().enumerate() {
            tile[at + c * 2..at + c * 2 + 2].copy_from_slice(&v.to_be_bytes());
        }
    } else {
        for (c, v) in pixel.into_iter().enumerate() {
            tile[at + c] = crate::raster::project16(v);
        }
    }
}
