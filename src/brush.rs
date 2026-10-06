//! Shared, bounded brush rasterization. Opacity caps a stroke; flow accumulates per dab.
use crate::raster::{blend, Pixel, Raster, TILE};
use serde_json::Value;
use std::{
    collections::{btree_map::Entry, BTreeMap},
    sync::Arc,
};

#[derive(Clone, Copy, Debug)]
pub struct Settings {
    pub radius: f32,
    pub hardness: f32,
    pub opacity: f32,
    pub flow: f32,
    pub spacing: f32,
    pub roundness: f32,
    pub angle: f32,
    pub smoothing: f32,
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            radius: 18.0,
            hardness: 1.0,
            opacity: 1.0,
            flow: 1.0,
            spacing: 0.15,
            roundness: 1.0,
            angle: 0.0,
            smoothing: 0.0,
        }
    }
}
impl Settings {
    pub fn from_command(c: &Value) -> Result<Self, String> {
        let parameter = |key: &str, default: f32, min: f32, max: f32| {
            let v = match c.get(key) {
                Some(v) => v.as_f64().ok_or("Invalid brush setting")?,
                None => default as f64,
            };
            if !v.is_finite() || v < min as f64 || v > max as f64 {
                return Err(format!("Brush {key} must be between {min} and {max}"));
            }
            Ok(v as f32)
        };
        Ok(Self {
            radius: parameter("radius", 10.0, 0.5, 512.0)?,
            hardness: parameter(
                "hardness",
                if c["soft"].as_bool().unwrap_or(false) {
                    0.5
                } else {
                    1.0
                },
                0.0,
                1.0,
            )?,
            opacity: parameter("opacity", 1.0, 0.0, 1.0)?,
            flow: parameter("flow", 1.0, 0.0, 1.0)?,
            spacing: parameter("spacing", 0.15, 0.01, 2.0)?,
            roundness: parameter("roundness", 1.0, 0.05, 1.0)?,
            angle: parameter("angle", 0.0, -180.0, 180.0)?,
            smoothing: parameter("smoothing", 0.0, 0.0, 1.0)?,
        })
    }
    pub fn coverage(self, dx: f32, dy: f32) -> f32 {
        Tip::new(self).coverage(dx, dy)
    }
}

/// Preparing once avoids repeating trigonometry for every pixel in a stroke.
#[derive(Clone, Copy)]
struct Tip {
    settings: Settings,
    sin: f32,
    cos: f32,
    solid_distance: f32,
    outside_distance: f32,
}
impl Tip {
    fn new(settings: Settings) -> Self {
        let (sin, cos) = settings.angle.to_radians().sin_cos();
        // The gradient is bounded by the larger inverse ellipse radius. Leave
        // a generous margin so early decisions do not touch the antialiased edge.
        let gradient_limit = (1.0 / settings.radius)
            .abs()
            .max((1.0 / (settings.radius * settings.roundness)).abs())
            * 1.01;
        Self {
            settings,
            sin,
            cos,
            solid_distance: settings.hardness.min(1.0 - gradient_limit),
            outside_distance: 1.0 + gradient_limit,
        }
    }
    fn coverage(self, dx: f32, dy: f32) -> f32 {
        let settings = self.settings;
        let u = (dx * self.cos + dy * self.sin) / settings.radius;
        let v = (-dx * self.sin + dy * self.cos) / (settings.radius * settings.roundness);
        let distance = (u * u + v * v).sqrt();
        if distance <= self.solid_distance {
            return 1.0;
        }
        if distance >= self.outside_distance {
            return 0.0;
        }
        let gradient = if distance > 0.0001 {
            (u / settings.radius).hypot(v / (settings.radius * settings.roundness)) / distance
        } else {
            1.0 / settings.radius
        };
        let edge = (1.0 - settings.hardness).max(gradient);
        ((1.0 - distance + gradient * 0.5) / edge).clamp(0.0, 1.0)
    }
}

pub fn paint(
    raster: &mut Raster,
    points: &[[f32; 2]],
    settings: Settings,
    color: Pixel,
    erase: bool,
    clip: Option<[i32; 4]>,
) -> Result<(), String> {
    if points.is_empty()
        || points.len() > 10000
        || points
            .iter()
            .flatten()
            .any(|v| !v.is_finite() || v.abs() > 100000.0)
    {
        return Err("Invalid stroke points".into());
    }
    if settings.opacity == 0.0 || settings.flow == 0.0 || color[3] == 0 {
        return Ok(());
    }
    let clip = clip.unwrap_or([0, 0, raster.width as i32, raster.height as i32]);
    let clip = [
        clip[0].max(0),
        clip[1].max(0),
        clip[2].min(raster.width as i32),
        clip[3].min(raster.height as i32),
    ];
    let step = (settings.radius * 2.0 * settings.spacing).max(0.5);
    let mut dabs = vec![points[0]];
    let mut previous = points[0];
    let mut until_next = step;
    for (index, &point) in points.iter().enumerate().skip(1) {
        let weight = if index + 1 == points.len() {
            1.0
        } else {
            1.0 - settings.smoothing * 0.9
        };
        let target = [
            previous[0] + (point[0] - previous[0]) * weight,
            previous[1] + (point[1] - previous[1]) * weight,
        ];
        let dx = target[0] - previous[0];
        let dy = target[1] - previous[1];
        let distance = dx.hypot(dy);
        let mut along = until_next;
        while along <= distance {
            dabs.push([
                previous[0] + dx * along / distance,
                previous[1] + dy * along / distance,
            ]);
            if dabs.len() > 16000 {
                return Err("Stroke is too long; split it into shorter strokes".into());
            }
            along += step;
        }
        until_next = along - distance;
        previous = target;
    }
    // Reject pathological work before allocating or editing. Clipping also skips off-canvas dabs.
    let bounds = |p: [f32; 2]| {
        [
            (p[0] - settings.radius - 1.0).floor() as i32,
            (p[1] - settings.radius - 1.0).floor() as i32,
            (p[0] + settings.radius + 1.0).ceil() as i32,
            (p[1] + settings.radius + 1.0).ceil() as i32,
        ]
    };
    let mut work = 0u64;
    for &p in &dabs {
        let b = bounds(p);
        work += (b[2].min(clip[2]) - b[0].max(clip[0])).max(0) as u64
            * (b[3].min(clip[3]) - b[1].max(clip[1])).max(0) as u64;
        if work > 250_000_000 {
            return Err(
                "Stroke exceeds the work budget; use shorter strokes or wider spacing".into(),
            );
        }
    }
    // Coverage has only alpha. Resolve the tile once per dab/tile intersection rather
    // than looking it up and performing copy-on-write for every covered pixel.
    let mut coverage: BTreeMap<(u32, u32), Vec<u8>> = BTreeMap::new();
    let tip = Tip::new(settings);
    for p in dabs {
        let b = bounds(p);
        let left = b[0].max(clip[0]);
        let top = b[1].max(clip[1]);
        let right = b[2].min(clip[2]);
        let bottom = b[3].min(clip[3]);
        if left >= right || top >= bottom {
            continue;
        }
        for ty in top as u32 / TILE..=(bottom - 1) as u32 / TILE {
            for tx in left as u32 / TILE..=(right - 1) as u32 / TILE {
                let tile = coverage
                    .entry((tx, ty))
                    .or_insert_with(|| vec![0; (TILE * TILE) as usize]);
                let tile_x = (tx * TILE) as i32;
                let tile_y = (ty * TILE) as i32;
                let x0 = left.max(tile_x);
                let x1 = right.min(tile_x + TILE as i32);
                let y0 = top.max(tile_y);
                let y1 = bottom.min(tile_y + TILE as i32);
                for y in y0..y1 {
                    let row = ((y - tile_y) as u32 * TILE) as usize;
                    for x in x0..x1 {
                        let alpha = tip.coverage(x as f32 + 0.5 - p[0], y as f32 + 0.5 - p[1])
                            * settings.flow;
                        if alpha <= 0.0 {
                            continue;
                        }
                        let alpha_byte = &mut tile[row + (x - tile_x) as usize];
                        let old = *alpha_byte as f32 / 255.0;
                        *alpha_byte = ((old + alpha * (1.0 - old)) * 255.0).round() as u8;
                    }
                }
            }
        }
    }
    for (key, alpha) in coverage {
        if !alpha.iter().any(|&value| value != 0) {
            continue;
        }
        match raster.tiles.entry(key) {
            Entry::Occupied(mut entry) => {
                apply_coverage(
                    Arc::<Vec<u8>>::make_mut(entry.get_mut()).as_mut_slice(),
                    &alpha,
                    settings.opacity,
                    color,
                    erase,
                );
            }
            Entry::Vacant(entry) => {
                let mut tile = vec![0; (TILE * TILE * 4) as usize];
                if apply_coverage(&mut tile, &alpha, settings.opacity, color, erase) {
                    entry.insert(Arc::new(tile));
                }
            }
        }
    }
    Ok(())
}

/// Returns whether a previously empty destination contains any nonzero pixel.
fn apply_coverage(
    tile: &mut [u8],
    coverage: &[u8],
    opacity: f32,
    color: Pixel,
    erase: bool,
) -> bool {
    let mut nonzero = false;
    for (pixel, &coverage) in tile.chunks_exact_mut(4).zip(coverage) {
        if coverage == 0 {
            continue;
        }
        let alpha = coverage as f32 / 255.0 * opacity;
        let old = [pixel[0], pixel[1], pixel[2], pixel[3]];
        let mut src = color;
        src[3] = (color[3] as f32 * alpha).round() as u8;
        let result = if erase {
            [
                old[0],
                old[1],
                old[2],
                (old[3] as f32 * (1.0 - src[3] as f32 / 255.0)).round() as u8,
            ]
        } else {
            blend(old, src, 1.0, "normal")
        };
        nonzero |= result != [0; 4];
        pixel.copy_from_slice(&result);
    }
    nonzero
}
