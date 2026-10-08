//! Shared, bounded brush rasterization. Opacity caps a stroke; flow accumulates per dab.
use crate::raster::{blend, Pixel, Raster, TILE};
use serde_json::Value;
use std::{
    collections::{btree_map::Entry, BTreeMap, BTreeSet},
    sync::Arc,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TipKind {
    Round,
    Dry,
    Chalk,
    Grain,
    Bristle,
}
impl TipKind {
    pub fn name(self) -> &'static str {
        match self {
            Self::Round => "round",
            Self::Dry => "dry",
            Self::Chalk => "chalk",
            Self::Grain => "grain",
            Self::Bristle => "bristle",
        }
    }
    fn parse(name: &str) -> Result<Self, String> {
        match name {
            "round" => Ok(Self::Round),
            "dry" => Ok(Self::Dry),
            "chalk" => Ok(Self::Chalk),
            "grain" => Ok(Self::Grain),
            "bristle" => Ok(Self::Bristle),
            _ => Err("Choose round, dry, chalk, grain or bristle tip".into()),
        }
    }
}

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
    pub tip: TipKind,
    pub density: f32,
    pub grain: f32,
    pub seed: u32,
    pub pressure_size: bool,
    pub pressure_opacity: bool,
    pub taper_start: f32,
    pub taper_end: f32,
    pub taper_size: bool,
    pub taper_opacity: bool,
    pub wetness: f32,
    pub load: f32,
    pub pickup: f32,
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
            tip: TipKind::Round,
            density: 1.0,
            grain: 2.0,
            seed: 0,
            pressure_size: true,
            pressure_opacity: true,
            taper_start: 0.0,
            taper_end: 0.0,
            taper_size: true,
            taper_opacity: true,
            wetness: 0.8,
            load: 0.0,
            pickup: 0.25,
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
        let boolean = |key: &str, default: bool| -> Result<bool, String> {
            c.get(key).map_or(Ok(default), |v| {
                v.as_bool()
                    .ok_or_else(|| format!("Brush {key} must be true or false"))
            })
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
            tip: TipKind::parse(
                c.get("tip")
                    .map_or(Ok("round"), |v| v.as_str().ok_or("Brush tip must be text"))?,
            )?,
            density: parameter("density", 1.0, 0.05, 1.0)?,
            grain: parameter("grain", 2.0, 0.5, 32.0)?,
            seed: c.get("seed").map_or(Ok(0), |v| {
                v.as_u64()
                    .and_then(|n| u32::try_from(n).ok())
                    .ok_or("Brush seed must be a 32-bit unsigned integer")
            })?,
            pressure_size: boolean("pressure_size", true)?,
            pressure_opacity: boolean("pressure_opacity", true)?,
            taper_start: parameter("taper_start", 0.0, 0.0, 10000.0)?,
            taper_end: parameter("taper_end", 0.0, 0.0, 10000.0)?,
            taper_size: boolean("taper_size", true)?,
            taper_opacity: boolean("taper_opacity", true)?,
            wetness: parameter("wetness", 0.8, 0.0, 1.0)?,
            load: parameter("load", 0.0, 0.0, 1.0)?,
            pickup: parameter("pickup", 0.25, 0.0, 1.0)?,
        })
    }
    pub fn coverage(self, dx: f32, dy: f32) -> f32 {
        Tip::new(self).coverage(dx, dy)
    }
    fn validate(self) -> Result<(), String> {
        for (name, value, min, max) in [
            ("radius", self.radius, 0.5, 512.0),
            ("hardness", self.hardness, 0.0, 1.0),
            ("opacity", self.opacity, 0.0, 1.0),
            ("flow", self.flow, 0.0, 1.0),
            ("spacing", self.spacing, 0.01, 2.0),
            ("roundness", self.roundness, 0.05, 1.0),
            ("angle", self.angle, -180.0, 180.0),
            ("smoothing", self.smoothing, 0.0, 1.0),
            ("density", self.density, 0.05, 1.0),
            ("grain", self.grain, 0.5, 32.0),
            ("taper_start", self.taper_start, 0.0, 10000.0),
            ("taper_end", self.taper_end, 0.0, 10000.0),
            ("wetness", self.wetness, 0.0, 1.0),
            ("load", self.load, 0.0, 1.0),
            ("pickup", self.pickup, 0.0, 1.0),
        ] {
            if !value.is_finite() || !(min..=max).contains(&value) {
                return Err(format!("Brush {name} must be between {min} and {max}"));
            }
        }
        Ok(())
    }
}

/// Preparing once avoids repeating trigonometry for every pixel in a stroke.
#[derive(Clone, Copy)]
pub(crate) struct Tip {
    settings: Settings,
    sin: f32,
    cos: f32,
    solid_distance: f32,
    outside_distance: f32,
}
impl Tip {
    pub(crate) fn new(settings: Settings) -> Self {
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
    pub(crate) fn with_radius(mut self, radius: f32) -> Self {
        self.settings.radius = radius;
        let gradient_limit = (1.0 / radius)
            .abs()
            .max((1.0 / (radius * self.settings.roundness)).abs())
            * 1.01;
        self.solid_distance = self.settings.hardness.min(1.0 - gradient_limit);
        self.outside_distance = 1.0 + gradient_limit;
        self
    }
    pub(crate) fn at(self, point: [f32; 2], x: i32, y: i32) -> f32 {
        let dx = x as f32 + 0.5 - point[0];
        let dy = y as f32 + 0.5 - point[1];
        let coverage = self.coverage(dx, dy);
        if coverage == 0.0 || self.settings.tip == TipKind::Round {
            return coverage;
        }
        let s = self.settings;
        let u = dx * self.cos + dy * self.sin;
        let v = -dx * self.sin + dy * self.cos;
        let grain = s.grain;
        let noise = noise2((x as f32 + 0.5) / grain, (y as f32 + 0.5) / grain, s.seed);
        let lane = noise2(0.0, (v / grain).floor(), s.seed.wrapping_add(83));
        let fiber = ((v / grain).rem_euclid(1.0) - 0.5).abs();
        let texture = match s.tip {
            TipKind::Round => 1.0,
            TipKind::Grain => {
                if noise <= s.density {
                    0.25 + 0.75 * noise / s.density
                } else {
                    0.0
                }
            }
            TipKind::Chalk => ((s.density
                - smooth_noise((x as f32 + 0.5) / grain, (y as f32 + 0.5) / grain, s.seed))
                / s.density
                * 2.5)
                .clamp(0.0, 1.0),
            TipKind::Bristle => {
                if lane < s.density {
                    ((0.35 - fiber) * grain + 0.5).clamp(0.0, 1.0)
                        * (0.5
                            + 0.5 * noise2((u / grain * 0.25).floor(), (v / grain).floor(), s.seed))
                } else {
                    0.0
                }
            }
            TipKind::Dry => {
                if lane < s.density && noise < (s.density + 0.2).min(1.0) {
                    ((0.3 - fiber) * grain + 0.5).clamp(0.0, 1.0) * (0.25 + 0.75 * noise)
                } else {
                    0.0
                }
            }
        };
        coverage * texture
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

fn noise2(x: f32, y: f32, seed: u32) -> f32 {
    let mut h = (x.floor() as i32 as u32).wrapping_mul(0x8da6b343)
        ^ (y.floor() as i32 as u32).wrapping_mul(0xd8163841)
        ^ seed.wrapping_mul(0xcb1ab31f);
    h ^= h >> 16;
    h = h.wrapping_mul(0x7feb352d);
    h ^= h >> 15;
    h = h.wrapping_mul(0x846ca68b);
    h ^= h >> 16;
    (h >> 8) as f32 / 16777216.0
}
fn smooth_noise(x: f32, y: f32, seed: u32) -> f32 {
    let a = x.floor();
    let b = y.floor();
    let t = x - a;
    let u = y - b;
    let t = t * t * (3.0 - 2.0 * t);
    let u = u * u * (3.0 - 2.0 * u);
    let low = noise2(a, b, seed) * (1.0 - t) + noise2(a + 1.0, b, seed) * t;
    let high = noise2(a, b + 1.0, seed) * (1.0 - t) + noise2(a + 1.0, b + 1.0, seed) * t;
    low * (1.0 - u) + high * u
}

/// Pressure can be a third point coordinate or a parallel array. Mouse callers omit it.
pub fn points_from_command(c: &Value) -> Result<(Vec<[f32; 2]>, Option<Vec<f32>>), String> {
    let values = c["points"]
        .as_array()
        .filter(|p| !p.is_empty() && p.len() <= 10000)
        .ok_or("Stroke needs 1–10000 points")?;
    let mut points = Vec::with_capacity(values.len());
    let mut pressures = Vec::with_capacity(values.len());
    let mut explicit = false;
    for value in values {
        let point = value
            .as_array()
            .filter(|p| p.len() == 2 || p.len() == 3)
            .ok_or("Stroke points must be [x,y] or [x,y,pressure]")?;
        let coordinate = |index: usize| -> Result<f32, String> {
            let n = point[index]
                .as_f64()
                .filter(|n| n.is_finite() && n.abs() <= 100000.0)
                .ok_or("Invalid stroke coordinate")?;
            Ok(n as f32)
        };
        points.push([coordinate(0)?, coordinate(1)?]);
        if point.len() == 3 {
            explicit = true;
            pressures.push(pressure(&point[2])?);
        } else {
            pressures.push(1.0);
        }
    }
    if let Some(values) = c.get("pressures") {
        if explicit {
            return Err("Use pressure coordinates or pressures array, not both".into());
        }
        let values = values
            .as_array()
            .filter(|p| p.len() == points.len())
            .ok_or("Pressures must match the point count")?;
        pressures = values.iter().map(pressure).collect::<Result<Vec<_>, _>>()?;
        explicit = true;
    }
    Ok((points, explicit.then_some(pressures)))
}
fn pressure(value: &Value) -> Result<f32, String> {
    value
        .as_f64()
        .filter(|n| n.is_finite() && (0.0..=1.0).contains(n))
        .map(|n| n as f32)
        .ok_or_else(|| "Pressure must be between 0 and 1".into())
}

#[derive(Clone, Copy, PartialEq)]
pub(crate) struct Dab {
    pub point: [f32; 2],
    pub radius: f32,
    pub opacity: f32,
}

pub(crate) fn dabs(
    points: &[[f32; 2]],
    pressures: Option<&[f32]>,
    settings: Settings,
) -> Result<Vec<Dab>, String> {
    settings.validate()?;
    if points.is_empty()
        || points.len() > 10000
        || points
            .iter()
            .flatten()
            .any(|v| !v.is_finite() || v.abs() > 100000.0)
    {
        return Err("Invalid stroke points".into());
    }
    if pressures.is_some_and(|p| {
        p.len() != points.len() || p.iter().any(|v| !v.is_finite() || !(0.0..=1.0).contains(v))
    }) {
        return Err("Pressures must match points and stay between 0 and 1".into());
    }
    let step = (settings.radius * 2.0 * settings.spacing).max(0.5);
    let at_pressure = |index: usize| pressures.map_or(1.0, |p| p[index]);
    let mut raw = vec![(points[0], 0.0, at_pressure(0))];
    let mut previous = points[0];
    let mut until_next = step;
    let mut length = 0.0;
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
            raw.push((
                [
                    previous[0] + dx * along / distance,
                    previous[1] + dy * along / distance,
                ],
                length + along,
                at_pressure(index - 1)
                    + (at_pressure(index) - at_pressure(index - 1)) * along / distance,
            ));
            if raw.len() > 16000 {
                return Err("Stroke is too long; split it into shorter strokes".into());
            }
            along += step;
        }
        length += distance;
        until_next = along - distance;
        previous = target;
    }
    Ok(raw
        .into_iter()
        .map(|(point, distance, p)| {
            let smooth = |v: f32| {
                let v = v.clamp(0.0, 1.0);
                v * v * (3.0 - 2.0 * v)
            };
            let start = if settings.taper_start > 0.0 && length > 0.0 {
                smooth(distance / settings.taper_start)
            } else {
                1.0
            };
            let end = if settings.taper_end > 0.0 && length > 0.0 {
                smooth((length - distance) / settings.taper_end)
            } else {
                1.0
            };
            let taper = start.min(end);
            let size_pressure = if settings.pressure_size { p } else { 1.0 };
            let opacity_pressure = if settings.pressure_opacity { p } else { 1.0 };
            let size_taper = if settings.taper_size { taper } else { 1.0 };
            let opacity_taper = if settings.taper_opacity { taper } else { 1.0 };
            Dab {
                point,
                radius: settings.radius * size_pressure * size_taper,
                opacity: opacity_pressure * opacity_taper,
            }
        })
        .collect())
}

pub(crate) fn dab_bounds(dab: Dab) -> [i32; 4] {
    [
        (dab.point[0] - dab.radius - 1.0).floor() as i32,
        (dab.point[1] - dab.radius - 1.0).floor() as i32,
        (dab.point[0] + dab.radius + 1.0).ceil() as i32,
        (dab.point[1] + dab.radius + 1.0).ceil() as i32,
    ]
}
pub(crate) fn work_budget(dabs: &[Dab], clip: [i32; 4]) -> Result<(), String> {
    let mut work = 0u64;
    for &dab in dabs {
        let b = dab_bounds(dab);
        work += (b[2].min(clip[2]) - b[0].max(clip[0])).max(0) as u64
            * (b[3].min(clip[3]) - b[1].max(clip[1])).max(0) as u64;
        if work > 250_000_000 {
            return Err(
                "Stroke exceeds the work budget; use shorter strokes or wider spacing".into(),
            );
        }
    }
    Ok(())
}

/// Shared pressure/texture/flow coverage for tools that sample pixels instead of pigment.
pub(crate) fn sampled_coverage(
    raster: &Raster,
    points: &[[f32; 2]],
    pressures: Option<&[f32]>,
    settings: Settings,
    clip: Option<[i32; 4]>,
    polygon: Option<&[[f32; 2]]>,
) -> Result<BTreeMap<(u32, u32), Vec<u16>>, String> {
    let dabs = dabs(points, pressures, settings)?;
    let area = clip.unwrap_or([0, 0, raster.width as i32, raster.height as i32]);
    let area = [
        area[0].max(0),
        area[1].max(0),
        area[2].min(raster.width as i32),
        area[3].min(raster.height as i32),
    ];
    work_budget(&dabs, area)?;
    let mut coverage = BTreeMap::new();
    if settings.opacity > 0. && settings.flow > 0. {
        accumulate16(&mut coverage, &dabs, settings, area, polygon);
    }
    Ok(coverage)
}

pub fn paint(
    raster: &mut Raster,
    points: &[[f32; 2]],
    settings: Settings,
    color: Pixel,
    erase: bool,
    clip: Option<[i32; 4]>,
) -> Result<(), String> {
    paint_with_selection(raster, points, settings, color, erase, clip, None)
}

/// Polygon vertices are layer-local; only selected pixel centers receive coverage.
pub fn paint_with_selection(
    raster: &mut Raster,
    points: &[[f32; 2]],
    settings: Settings,
    color: Pixel,
    erase: bool,
    clip: Option<[i32; 4]>,
    polygon: Option<&[[f32; 2]]>,
) -> Result<(), String> {
    paint_with_pressure(raster, points, None, settings, color, erase, clip, polygon)
}

pub fn paint_with_pressure(
    raster: &mut Raster,
    points: &[[f32; 2]],
    pressures: Option<&[f32]>,
    settings: Settings,
    color: Pixel,
    erase: bool,
    clip: Option<[i32; 4]>,
    polygon: Option<&[[f32; 2]]>,
) -> Result<(), String> {
    let dabs = dabs(points, pressures, settings)?;
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
    // Reject pathological work before allocating or editing. Clipping also skips off-canvas dabs.
    work_budget(&dabs, clip)?;
    if raster.depth == 16 {
        let mut coverage = BTreeMap::new();
        accumulate16(&mut coverage, &dabs, settings, clip, polygon);
        let color = color.map(|value| value as u16 * 257);
        for (key, alpha) in coverage {
            if !alpha.iter().any(|&value| value != 0) {
                continue;
            }
            match raster.samples16.entry(key) {
                Entry::Occupied(mut entry) => {
                    apply_coverage16(
                        Arc::<Vec<u16>>::make_mut(entry.get_mut()).as_mut_slice(),
                        &alpha,
                        settings.opacity,
                        color,
                        erase,
                    );
                }
                Entry::Vacant(entry) => {
                    let mut tile = vec![0; (TILE * TILE * 4) as usize];
                    if apply_coverage16(&mut tile, &alpha, settings.opacity, color, erase) {
                        entry.insert(Arc::new(tile));
                    }
                }
            }
        }
        return Ok(());
    }
    // Coverage has only alpha. Resolve the tile once per dab/tile intersection rather
    // than looking it up and performing copy-on-write for every covered pixel.
    let mut coverage: BTreeMap<(u32, u32), Vec<u8>> = BTreeMap::new();
    accumulate(&mut coverage, &dabs, settings, clip, polygon);
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

fn accumulate(
    coverage: &mut BTreeMap<(u32, u32), Vec<u8>>,
    dabs: &[Dab],
    settings: Settings,
    clip: [i32; 4],
    polygon: Option<&[[f32; 2]]>,
) -> BTreeSet<(u32, u32)> {
    let mut touched = BTreeSet::new();
    let base_tip = Tip::new(settings);
    for &dab in dabs {
        if dab.radius <= 0.001 || dab.opacity == 0.0 {
            continue;
        }
        let tip = base_tip.with_radius(dab.radius);
        let b = dab_bounds(dab);
        let left = b[0].max(clip[0]);
        let top = b[1].max(clip[1]);
        let right = b[2].min(clip[2]);
        let bottom = b[3].min(clip[3]);
        if left >= right || top >= bottom {
            continue;
        }
        for ty in top as u32 / TILE..=(bottom - 1) as u32 / TILE {
            for tx in left as u32 / TILE..=(right - 1) as u32 / TILE {
                touched.insert((tx, ty));
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
                        if polygon.is_some_and(|selection| {
                            !crate::selection::contains(selection, x as f32 + 0.5, y as f32 + 0.5)
                        }) {
                            continue;
                        }
                        let alpha = tip.at(dab.point, x, y) * settings.flow;
                        if alpha <= 0.0 {
                            continue;
                        }
                        let alpha_byte = &mut tile[row + (x - tile_x) as usize];
                        let old = *alpha_byte as f32 / 255.0;
                        *alpha_byte =
                            ((old + alpha * (dab.opacity - old).max(0.0)) * 255.0).round() as u8;
                    }
                }
            }
        }
    }
    touched
}

fn apply_coverage16(
    tile: &mut [u16],
    coverage: &[u16],
    opacity: f32,
    color: [u16; 4],
    erase: bool,
) -> bool {
    let mut nonzero = false;
    for (pixel, &coverage) in tile.chunks_exact_mut(4).zip(coverage) {
        if coverage == 0 {
            continue;
        }
        let alpha = coverage as f64 / 65535.0 * opacity as f64;
        let old = [pixel[0], pixel[1], pixel[2], pixel[3]];
        let mut src = color;
        src[3] = (color[3] as f64 * alpha).round() as u16;
        let result = if erase {
            [
                old[0],
                old[1],
                old[2],
                (old[3] as f64 * (1.0 - src[3] as f64 / 65535.0)).round() as u16,
            ]
        } else {
            crate::raster::blend16(old, src, 1.0, "normal")
        };
        nonzero |= result != [0; 4];
        pixel.copy_from_slice(&result);
    }
    nonzero
}

fn accumulate16(
    coverage: &mut BTreeMap<(u32, u32), Vec<u16>>,
    dabs: &[Dab],
    settings: Settings,
    clip: [i32; 4],
    polygon: Option<&[[f32; 2]]>,
) -> BTreeSet<(u32, u32)> {
    let mut touched = BTreeSet::new();
    let base_tip = Tip::new(settings);
    for &dab in dabs {
        if dab.radius <= 0.001 || dab.opacity == 0.0 {
            continue;
        }
        let tip = base_tip.with_radius(dab.radius);
        let b = dab_bounds(dab);
        let left = b[0].max(clip[0]);
        let top = b[1].max(clip[1]);
        let right = b[2].min(clip[2]);
        let bottom = b[3].min(clip[3]);
        if left >= right || top >= bottom {
            continue;
        }
        for ty in top as u32 / TILE..=(bottom - 1) as u32 / TILE {
            for tx in left as u32 / TILE..=(right - 1) as u32 / TILE {
                touched.insert((tx, ty));
                let tile = coverage
                    .entry((tx, ty))
                    .or_insert_with(|| vec![0; (TILE * TILE) as usize]);
                let tile_x = (tx * TILE) as i32;
                let tile_y = (ty * TILE) as i32;
                for y in top.max(tile_y)..bottom.min(tile_y + TILE as i32) {
                    let row = ((y - tile_y) as u32 * TILE) as usize;
                    for x in left.max(tile_x)..right.min(tile_x + TILE as i32) {
                        if polygon.is_some_and(|p| {
                            !crate::selection::contains(p, x as f32 + 0.5, y as f32 + 0.5)
                        }) {
                            continue;
                        }
                        let alpha = tip.at(dab.point, x, y) as f64 * settings.flow as f64;
                        if alpha <= 0.0 {
                            continue;
                        }
                        let byte = &mut tile[row + (x - tile_x) as usize];
                        let old = *byte as f64 / 65535.0;
                        *byte = ((old + alpha * (dab.opacity as f64 - old).max(0.0)) * 65535.0)
                            .round() as u16;
                    }
                }
            }
        }
    }
    touched
}

/// Exact incremental gesture coverage. Source and history tiles remain shared and immutable.
pub struct Session {
    baseline: Raster,
    raster: Raster,
    settings: Settings,
    color: Pixel,
    erase: bool,
    clip: [i32; 4],
    polygon: Option<Vec<[f32; 2]>>,
    dabs: Vec<Dab>,
    coverage: BTreeMap<(u32, u32), Vec<u8>>,
    coverage16: BTreeMap<(u32, u32), Vec<u16>>,
    last_update_dabs: usize,
}
impl Session {
    pub fn new(
        baseline: &Raster,
        settings: Settings,
        color: Pixel,
        erase: bool,
        clip: Option<[i32; 4]>,
        polygon: Option<&[[f32; 2]]>,
    ) -> Result<Self, String> {
        settings.validate()?;
        let clip = clip.unwrap_or([0, 0, baseline.width as i32, baseline.height as i32]);
        let clip = [
            clip[0].max(0),
            clip[1].max(0),
            clip[2].min(baseline.width as i32),
            clip[3].min(baseline.height as i32),
        ];
        Ok(Self {
            baseline: baseline.clone(),
            raster: baseline.clone(),
            settings,
            color,
            erase,
            clip,
            polygon: polygon.map(|points| points.to_vec()),
            dabs: vec![],
            coverage: BTreeMap::new(),
            coverage16: BTreeMap::new(),
            last_update_dabs: 0,
        })
    }
    pub fn last_update_dabs(&self) -> usize {
        self.last_update_dabs
    }
    pub fn update(
        &mut self,
        points: &[[f32; 2]],
        pressures: Option<&[f32]>,
    ) -> Result<&Raster, String> {
        let next = dabs(points, pressures, self.settings)?;
        let visible =
            self.settings.opacity != 0.0 && self.settings.flow != 0.0 && self.color[3] != 0;
        if visible {
            work_budget(&next, self.clip)?;
        }
        let prefix = if next.starts_with(&self.dabs) {
            self.dabs.len()
        } else {
            self.coverage.clear();
            self.coverage16.clear();
            self.raster = self.baseline.clone();
            0
        };
        self.last_update_dabs = next.len() - prefix;
        if visible && self.baseline.depth == 16 {
            let touched = accumulate16(
                &mut self.coverage16,
                &next[prefix..],
                self.settings,
                self.clip,
                self.polygon.as_deref(),
            );
            for key in touched {
                let alpha = &self.coverage16[&key];
                if !alpha.iter().any(|&value| value != 0) {
                    continue;
                }
                let old = self.baseline.samples16.get(&key);
                let mut tile = old.map_or_else(
                    || vec![0; (TILE * TILE * 4) as usize],
                    |tile| tile.as_ref().clone(),
                );
                let nonzero = apply_coverage16(
                    &mut tile,
                    alpha,
                    self.settings.opacity,
                    self.color.map(|value| value as u16 * 257),
                    self.erase,
                );
                if old.is_some() || nonzero {
                    self.raster.samples16.insert(key, Arc::new(tile));
                }
            }
            self.dabs = next;
            return Ok(&self.raster);
        }
        if visible {
            let touched = accumulate(
                &mut self.coverage,
                &next[prefix..],
                self.settings,
                self.clip,
                self.polygon.as_deref(),
            );
            for key in touched {
                let alpha = &self.coverage[&key];
                if !alpha.iter().any(|&value| value != 0) {
                    continue;
                }
                // Recompose from gesture start, rather than painting on last frame's result.
                // This retains one-stroke opacity caps and avoids double-applying erasure.
                let old = self.baseline.tiles.get(&key);
                let mut tile = old.map_or_else(
                    || vec![0; (TILE * TILE * 4) as usize],
                    |tile| tile.as_ref().clone(),
                );
                let nonzero = apply_coverage(
                    &mut tile,
                    alpha,
                    self.settings.opacity,
                    self.color,
                    self.erase,
                );
                if old.is_some() || nonzero {
                    self.raster.tiles.insert(key, Arc::new(tile));
                }
            }
        }
        self.dabs = next;
        Ok(&self.raster)
    }
}
