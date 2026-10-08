//! Deterministic wet pigment reservoir. The Engine owns locks, undo and gesture previews.
use crate::{
    brush::{self, Settings, Tip},
    raster::{Pixel, Raster, TILE},
};
use std::sync::Arc;

pub fn paint(
    raster: &mut Raster,
    points: &[[f32; 2]],
    pressures: Option<&[f32]>,
    settings: Settings,
    color: Pixel,
    clip: Option<[i32; 4]>,
    polygon: Option<&[[f32; 2]]>,
) -> Result<(), String> {
    let dabs = brush::dabs(points, pressures, settings)?;
    let clip = clip.unwrap_or([0, 0, raster.width as i32, raster.height as i32]);
    let clip = [
        clip[0].max(0),
        clip[1].max(0),
        clip[2].min(raster.width as i32),
        clip[3].min(raster.height as i32),
    ];
    brush::work_budget(&dabs, clip)?;
    if settings.opacity == 0.0 || settings.flow == 0.0 || settings.wetness == 0.0 {
        return Ok(());
    }
    if raster.depth == 16 {
        return paint16(raster, &dabs, settings, color, clip, polygon);
    }
    let selected = |x: i32, y: i32| {
        polygon.is_none_or(|p| crate::selection::contains(p, x as f32 + 0.5, y as f32 + 0.5))
    };
    let base_tip = Tip::new(settings);
    let mut carry: Option<[f32; 4]> = None;
    let mut changes = Vec::new();
    for dab in dabs {
        if dab.radius <= 0.001 || dab.opacity == 0.0 {
            continue;
        }
        let tip = base_tip.with_radius(dab.radius);
        let b = brush::dab_bounds(dab);
        let bounds = [
            b[0].max(clip[0]),
            b[1].max(clip[1]),
            b[2].min(clip[2]),
            b[3].min(clip[3]),
        ];
        if bounds[0] >= bounds[2] || bounds[1] >= bounds[3] {
            continue;
        }
        // Sample before writing this dab. Premultiplied pigment includes transparent holes.
        let mut average = [0.0f32; 4];
        let mut weight = 0.0;
        for y in bounds[1]..bounds[3] {
            for x in bounds[0]..bounds[2] {
                if !selected(x, y) {
                    continue;
                }
                let amount = tip.at(dab.point, x, y);
                if amount == 0.0 {
                    continue;
                }
                let pixel = premultiplied(raster.get(x, y));
                for channel in 0..4 {
                    average[channel] += pixel[channel] * amount;
                }
                weight += amount;
            }
        }
        if weight <= 0.0 {
            continue;
        }
        for channel in &mut average {
            *channel /= weight;
        }
        let Some(mut pigment) = carry else {
            let loaded = premultiplied(color);
            for channel in 0..4 {
                average[channel] += (loaded[channel] - average[channel]) * settings.load;
            }
            carry = Some(average);
            continue;
        };
        for channel in 0..4 {
            pigment[channel] += (average[channel] - pigment[channel]) * settings.pickup;
        }
        carry = Some(pigment);
        if pigment[3] <= 0.0 {
            continue;
        }
        for ty in bounds[1] as u32 / TILE..=(bounds[3] - 1) as u32 / TILE {
            for tx in bounds[0] as u32 / TILE..=(bounds[2] - 1) as u32 / TILE {
                let key = (tx, ty);
                let tile_x = (tx * TILE) as i32;
                let tile_y = (ty * TILE) as i32;
                changes.clear();
                let old_tile = raster.tiles.get(&key);
                for y in bounds[1].max(tile_y)..bounds[3].min(tile_y + TILE as i32) {
                    for x in bounds[0].max(tile_x)..bounds[2].min(tile_x + TILE as i32) {
                        if !selected(x, y) {
                            continue;
                        }
                        let amount = tip.at(dab.point, x, y)
                            * settings.flow
                            * settings.opacity
                            * settings.wetness
                            * dab.opacity;
                        if amount <= 0.0 {
                            continue;
                        }
                        let at = (((y - tile_y) as u32 * TILE + (x - tile_x) as u32) * 4) as usize;
                        let old = old_tile.map_or([0; 4], |tile| {
                            [tile[at], tile[at + 1], tile[at + 2], tile[at + 3]]
                        });
                        let mut mixed = premultiplied(old);
                        for channel in 0..4 {
                            mixed[channel] += (pigment[channel] - mixed[channel]) * amount;
                        }
                        let result = unpremultiplied(mixed);
                        if result != old {
                            changes.push((at, result));
                        }
                    }
                }
                if changes.is_empty() {
                    continue;
                }
                let tile = raster
                    .tiles
                    .entry(key)
                    .or_insert_with(|| Arc::new(vec![0; (TILE * TILE * 4) as usize]));
                let tile = Arc::make_mut(tile);
                for &(at, pixel) in &changes {
                    tile[at..at + 4].copy_from_slice(&pixel);
                }
            }
        }
    }
    Ok(())
}

fn premultiplied(pixel: Pixel) -> [f32; 4] {
    let alpha = pixel[3] as f32 / 255.0;
    [
        pixel[0] as f32 / 255.0 * alpha,
        pixel[1] as f32 / 255.0 * alpha,
        pixel[2] as f32 / 255.0 * alpha,
        alpha,
    ]
}
fn unpremultiplied(pixel: [f32; 4]) -> Pixel {
    if pixel[3] <= 0.000001 {
        return [0; 4];
    }
    [
        (pixel[0] / pixel[3] * 255.0).round().clamp(0.0, 255.0) as u8,
        (pixel[1] / pixel[3] * 255.0).round().clamp(0.0, 255.0) as u8,
        (pixel[2] / pixel[3] * 255.0).round().clamp(0.0, 255.0) as u8,
        (pixel[3] * 255.0).round().clamp(0.0, 255.0) as u8,
    ]
}

fn paint16(
    raster: &mut Raster,
    dabs: &[brush::Dab],
    settings: Settings,
    color: Pixel,
    clip: [i32; 4],
    polygon: Option<&[[f32; 2]]>,
) -> Result<(), String> {
    let selected = |x: i32, y: i32| {
        polygon.is_none_or(|p| crate::selection::contains(p, x as f32 + 0.5, y as f32 + 0.5))
    };
    let base_tip = Tip::new(settings);
    let mut carry: Option<[f64; 4]> = None;
    let mut changes = Vec::new();
    for dab in dabs {
        if dab.radius <= 0.001 || dab.opacity == 0.0 {
            continue;
        }
        let tip = base_tip.with_radius(dab.radius);
        let b = brush::dab_bounds(*dab);
        let bounds = [
            b[0].max(clip[0]),
            b[1].max(clip[1]),
            b[2].min(clip[2]),
            b[3].min(clip[3]),
        ];
        if bounds[0] >= bounds[2] || bounds[1] >= bounds[3] {
            continue;
        }
        // Sample before writing this dab. Premultiplied pigment includes transparent holes.
        let mut average = [0.0f64; 4];
        let mut weight = 0.0;
        for y in bounds[1]..bounds[3] {
            for x in bounds[0]..bounds[2] {
                if !selected(x, y) {
                    continue;
                }
                let amount = tip.at(dab.point, x, y) as f64;
                if amount == 0.0 {
                    continue;
                }
                let pixel = premultiplied16(raster.get16(x, y));
                for channel in 0..4 {
                    average[channel] += pixel[channel] * amount;
                }
                weight += amount;
            }
        }
        if weight <= 0.0 {
            continue;
        }
        for channel in &mut average {
            *channel /= weight;
        }
        let Some(mut pigment) = carry else {
            let loaded = premultiplied16(color.map(|value| value as u16 * 257));
            for channel in 0..4 {
                average[channel] += (loaded[channel] - average[channel]) * settings.load as f64;
            }
            carry = Some(average);
            continue;
        };
        for channel in 0..4 {
            pigment[channel] += (average[channel] - pigment[channel]) * settings.pickup as f64;
        }
        carry = Some(pigment);
        if pigment[3] <= 0.0 {
            continue;
        }
        for ty in bounds[1] as u32 / TILE..=(bounds[3] - 1) as u32 / TILE {
            for tx in bounds[0] as u32 / TILE..=(bounds[2] - 1) as u32 / TILE {
                let key = (tx, ty);
                let tile_x = (tx * TILE) as i32;
                let tile_y = (ty * TILE) as i32;
                changes.clear();
                let old_tile = raster.samples16.get(&key);
                for y in bounds[1].max(tile_y)..bounds[3].min(tile_y + TILE as i32) {
                    for x in bounds[0].max(tile_x)..bounds[2].min(tile_x + TILE as i32) {
                        if !selected(x, y) {
                            continue;
                        }
                        let amount = (tip.at(dab.point, x, y)
                            * settings.flow
                            * settings.opacity
                            * settings.wetness
                            * dab.opacity) as f64;
                        if amount <= 0.0 {
                            continue;
                        }
                        let at = (((y - tile_y) as u32 * TILE + (x - tile_x) as u32) * 4) as usize;
                        let old = old_tile.map_or([0; 4], |tile| {
                            [tile[at], tile[at + 1], tile[at + 2], tile[at + 3]]
                        });
                        let mut mixed = premultiplied16(old);
                        for channel in 0..4 {
                            mixed[channel] += (pigment[channel] - mixed[channel]) * amount;
                        }
                        let result = unpremultiplied16(mixed);
                        if result != old {
                            changes.push((at, result));
                        }
                    }
                }
                if changes.is_empty() {
                    continue;
                }
                let tile = raster
                    .samples16
                    .entry(key)
                    .or_insert_with(|| Arc::new(vec![0; (TILE * TILE * 4) as usize]));
                let tile = Arc::make_mut(tile);
                for &(at, pixel) in &changes {
                    tile[at..at + 4].copy_from_slice(&pixel);
                }
            }
        }
    }
    Ok(())
}

fn premultiplied16(pixel: [u16; 4]) -> [f64; 4] {
    let alpha = pixel[3] as f64 / 65535.0;
    [
        pixel[0] as f64 / 65535.0 * alpha,
        pixel[1] as f64 / 65535.0 * alpha,
        pixel[2] as f64 / 65535.0 * alpha,
        alpha,
    ]
}
fn unpremultiplied16(pixel: [f64; 4]) -> [u16; 4] {
    if pixel[3] <= 0.0 {
        return [0; 4];
    }
    [
        (pixel[0] / pixel[3] * 65535.0).round().clamp(0., 65535.) as u16,
        (pixel[1] / pixel[3] * 65535.0).round().clamp(0., 65535.) as u16,
        (pixel[2] / pixel[3] * 65535.0).round().clamp(0., 65535.) as u16,
        (pixel[3] * 65535.0).round().clamp(0., 65535.) as u16,
    ]
}
