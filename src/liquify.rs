//! Non-destructive local deformation. Build one backward displacement field, then sample once.
use crate::{
    effects::Image,
    raster::{check_size, Pixel},
};
use serde_json::{json, Value};

pub const MAX_STROKES: usize = 128;
pub const MAX_POINTS: usize = 8192;
const MAX_DABS: usize = 32768;
const MAX_GRID: usize = 16_000_000;
const MAX_WORK: usize = 64_000_000;
const MAX_SELECTION_WORK: usize = 256_000_000;
const SCRATCH_BUDGET: usize = 256 * 1024 * 1024;

pub fn defaults() -> Value {
    json!({"radius":40.0,"strength":1.0,"strokes":[]})
}
#[derive(Clone, Copy)]
enum Mode {
    Push,
    Expand,
    Pinch,
    Restore,
}
struct Stroke {
    mode: Mode,
    points: Vec<[f32; 2]>,
    radius: f32,
    strength: f32,
    selection: Option<[i32; 4]>,
    polygon: Option<Vec<[f32; 2]>>,
    coverage: Option<crate::selection::Coverage>,
}
impl Stroke {
    fn contains(&self, point: [f32; 2]) -> bool {
        self.coverage
            .as_ref()
            .is_none_or(|m| m.value(point[0].floor() as i32, point[1].floor() as i32) > 0.0)
            && self.selection.is_none_or(|b| {
                point[0] >= b[0] as f32
                    && point[1] >= b[1] as f32
                    && point[0] < b[2] as f32
                    && point[1] < b[3] as f32
            })
            && self
                .polygon
                .as_ref()
                .is_none_or(|p| crate::selection::contains(p, point[0], point[1]))
    }
}
fn number(v: &Value, key: &str, default: f32, min: f32, max: f32) -> Result<f32, String> {
    let n = v.get(key).map_or(Ok(default as f64), |n| {
        n.as_f64()
            .ok_or_else(|| format!("Liquify {key} must be a number"))
    })?;
    if !n.is_finite() || n < min as f64 || n > max as f64 {
        return Err(format!("Liquify {key} must be {min}–{max}"));
    }
    Ok(n as f32)
}
fn parse_points(v: &Value, min: usize, max: usize) -> Result<Vec<[f32; 2]>, String> {
    let a = v
        .as_array()
        .filter(|a| (min..=max).contains(&a.len()))
        .ok_or_else(|| format!("Liquify needs {min}–{max} points"))?;
    a.iter()
        .map(|p| {
            let p = p
                .as_array()
                .filter(|p| p.len() == 2)
                .ok_or("Liquify points must be [x,y]")?;
            let coordinate = |v: &Value| {
                v.as_f64()
                    .filter(|n| n.is_finite() && n.abs() <= 100000.0)
                    .map(|n| n as f32)
                    .ok_or("Invalid liquify coordinate")
            };
            Ok([coordinate(&p[0])?, coordinate(&p[1])?])
        })
        .collect::<Result<Vec<_>, &str>>()
        .map_err(str::to_string)
}
fn parse(settings: &Value) -> Result<Vec<Stroke>, String> {
    if !settings.is_object() {
        return Err("Liquify settings must be an object".into());
    }
    let radius = number(settings, "radius", 40.0, 0.5, 512.0)?;
    let amount = number(settings, "strength", 1.0, 0.0, 1.0)?;
    let Some(strokes) = settings.get("strokes") else {
        return Ok(vec![]);
    };
    let strokes = strokes
        .as_array()
        .filter(|s| s.len() <= MAX_STROKES)
        .ok_or("Liquify supports up to 128 editable strokes")?;
    let mut total = 0;
    let mut coverage_bytes = 0usize;
    strokes
        .iter()
        .map(|s| {
            if !s.is_object() {
                return Err("Liquify strokes must be objects".to_string());
            }
            let mode = match s.get("mode").map_or(Ok("push"), |v| {
                v.as_str().ok_or("Liquify mode must be a string")
            })? {
                "push" => Mode::Push,
                "expand" => Mode::Expand,
                "pinch" => Mode::Pinch,
                "restore" => Mode::Restore,
                _ => return Err("Liquify mode must be push, expand, pinch or restore".into()),
            };
            let points = parse_points(&s["points"], 1, 2048)?;
            total += points.len();
            if total > MAX_POINTS {
                return Err("Liquify supports up to 8192 total control points".into());
            }
            let selection = if let Some(v) = s.get("selection").filter(|v| !v.is_null()) {
                let v = v
                    .as_array()
                    .filter(|v| v.len() == 4)
                    .ok_or("Liquify selection must be [left,top,right,bottom]")?;
                let mut b = [0; 4];
                for (i, n) in v.iter().enumerate() {
                    b[i] = n
                        .as_i64()
                        .filter(|n| (-100000..=100000).contains(n))
                        .ok_or("Invalid liquify selection")? as i32;
                }
                if b[0] >= b[2] || b[1] >= b[3] {
                    return Err("Liquify selection must have positive area".into());
                }
                Some(b)
            } else {
                None
            };
            let polygon = if let Some(v) = s.get("polygon").filter(|v| !v.is_null()) {
                let polygon = parse_points(v, 3, 8192)?;
                let bounds = crate::selection::bounds(&polygon)?;
                if selection.is_some_and(|s| s != bounds) {
                    return Err("Liquify polygon must match its selection bounds".into());
                }
                Some(polygon)
            } else {
                None
            };
            let coverage = if s["coverage"].is_null() {
                None
            } else {
                let m: crate::selection::Coverage = serde_json::from_value(s["coverage"].clone())
                    .map_err(|_| "Invalid Liquify coverage")?;
                m.validate()?;
                coverage_bytes += m.mask.bytes();
                if coverage_bytes > 128 * 1024 * 1024 {
                    return Err(
                        "Liquify selection snapshots exceed the 128 MiB source budget".into(),
                    );
                }
                Some(m)
            };
            Ok(Stroke {
                coverage,
                mode,
                points,
                radius: number(s, "radius", radius, 0.5, 512.0)?,
                strength: number(s, "strength", 1.0, 0.0, 1.0)? * amount,
                selection,
                polygon,
            })
        })
        .collect()
}
pub fn validate(settings: &Value) -> Result<(), String> {
    parse(settings).map(|_| ())
}

struct Dab {
    center: [f32; 2],
    delta: [f32; 2],
    strength: f32,
    stroke: usize,
}
struct Plan {
    strokes: Vec<Stroke>,
    dabs: Vec<Dab>,
    bounds: [i32; 4],
    step: f32,
    width: usize,
    height: usize,
    bytes: usize,
    changes: usize,
    clips: Vec<usize>,
    unrestricted: bool,
}
fn clipped_segment(a: [f32; 2], b: [f32; 2], bounds: [f32; 4]) -> Option<([f32; 2], [f32; 2])> {
    let delta = [b[0] - a[0], b[1] - a[1]];
    let (mut lo, mut hi) = (0.0_f32, 1.0_f32);
    for (origin, d, min, max) in [
        (a[0], delta[0], bounds[0], bounds[2]),
        (a[1], delta[1], bounds[1], bounds[3]),
    ] {
        if d.abs() < 0.000001 {
            if origin < min || origin > max {
                return None;
            }
        } else {
            let (x, y) = ((min - origin) / d, (max - origin) / d);
            lo = lo.max(x.min(y));
            hi = hi.min(x.max(y));
        }
    }
    if hi < lo {
        return None;
    }
    Some((
        [a[0] + delta[0] * lo, a[1] + delta[1] * lo],
        [a[0] + delta[0] * hi, a[1] + delta[1] * hi],
    ))
}
fn plan(width: u32, height: u32, settings: &Value) -> Result<Plan, String> {
    check_size(width, height)?;
    let strokes = parse(settings)?;
    // Fixed pixel-center spacing keeps existing strokes stable when another brush size
    // is added. Only the influenced region needs storage, independent of canvas size.
    let step = 1.0;
    let mut bounds = [width as i32, height as i32, 0, 0];
    let mut dabs = vec![];
    let mut work = 0usize;
    let mut changes = 0usize;
    for (index, stroke) in strokes.iter().enumerate().filter(|(_, s)| s.strength > 0.0) {
        let r = stroke.radius;
        let clip = stroke
            .selection
            .or_else(|| {
                stroke
                    .polygon
                    .as_ref()
                    .and_then(|p| crate::selection::bounds(p).ok())
            })
            .unwrap_or([0, 0, width as i32, height as i32]);
        let clip = [
            clip[0].max(0),
            clip[1].max(0),
            clip[2].min(width as i32),
            clip[3].min(height as i32),
        ];
        if clip[0] >= clip[2] || clip[1] >= clip[3] {
            continue;
        }
        let expanded = [
            clip[0] as f32 - r,
            clip[1] as f32 - r,
            clip[2] as f32 + r,
            clip[3] as f32 + r,
        ];
        let spacing = (r * 0.25).max(0.5);
        let mut add = |center: [f32; 2], delta: [f32; 2], strength: f32| -> Result<(), String> {
            if dabs.len() >= MAX_DABS {
                return Err(
                    "Liquify stroke replay exceeds the 32768-dab limit; split it into effects"
                        .into(),
                );
            }
            let b = [
                (center[0] - r - step).floor() as i32,
                (center[1] - r - step).floor() as i32,
                (center[0] + r + step).ceil() as i32,
                (center[1] + r + step).ceil() as i32,
            ];
            let b = [
                b[0].max(clip[0]),
                b[1].max(clip[1]),
                b[2].min(clip[2]),
                b[3].min(clip[3]),
            ];
            if b[0] >= b[2] || b[1] >= b[3] {
                return Ok(());
            }
            bounds = [
                bounds[0].min(b[0]),
                bounds[1].min(b[1]),
                bounds[2].max(b[2]),
                bounds[3].max(b[3]),
            ];
            let nodes = (((b[2] - b[0]) as f32 / step).ceil() as usize + 2)
                * (((b[3] - b[1]) as f32 / step).ceil() as usize + 2);
            work += nodes * stroke.polygon.as_ref().map_or(1, |p| p.len().max(1));
            changes = changes.max(nodes);
            if work > MAX_WORK {
                return Err(
                    "Liquify stroke replay exceeds the bounded work budget; split it into effects"
                        .into(),
                );
            }
            dabs.push(Dab {
                center,
                delta,
                strength,
                stroke: index,
            });
            Ok(())
        };
        if !matches!(stroke.mode, Mode::Push) {
            let first = stroke.points[0];
            if first[0] >= expanded[0]
                && first[1] >= expanded[1]
                && first[0] <= expanded[2]
                && first[1] <= expanded[3]
            {
                add(first, [0.0, 0.0], stroke.strength)?;
            }
        }
        for pair in stroke.points.windows(2) {
            let Some((a, b)) = clipped_segment(pair[0], pair[1], expanded) else {
                continue;
            };
            let length = (b[0] - a[0]).hypot(b[1] - a[1]);
            if length < 0.00001 {
                continue;
            }
            let count = (length / spacing).ceil().max(1.0) as usize;
            if count > MAX_DABS {
                return Err(
                    "Liquify stroke replay exceeds the 32768-dab limit; shorten the stroke".into(),
                );
            }
            let delta = [(b[0] - a[0]) / count as f32, (b[1] - a[1]) / count as f32];
            let strength = if matches!(stroke.mode, Mode::Push) {
                stroke.strength
            } else {
                stroke.strength * (length / count as f32 / spacing).min(1.0)
            };
            for i in 0..count {
                add(
                    [
                        a[0] + delta[0] * (i as f32 + 0.5),
                        a[1] + delta[1] * (i as f32 + 0.5),
                    ],
                    delta,
                    strength,
                )?;
            }
        }
    }
    if dabs.is_empty() {
        return Ok(Plan {
            strokes,
            dabs,
            bounds: [0; 4],
            step,
            width: 0,
            height: 0,
            bytes: 0,
            changes: 0,
            clips: vec![],
            unrestricted: true,
        });
    }
    let gw = ((bounds[2] - bounds[0]) as f32 / step).ceil() as usize + 1;
    let gh = ((bounds[3] - bounds[1]) as f32 / step).ceil() as usize + 1;
    if gw * gh > MAX_GRID {
        return Err("Liquify displacement region exceeds the 16-million-node budget; split distant strokes into effects".into());
    }
    let pixels = (bounds[2] - bounds[0]) as usize * (bounds[3] - bounds[1]) as usize;
    let active = dabs
        .iter()
        .map(|d| d.stroke)
        .collect::<std::collections::HashSet<_>>();
    let unrestricted = active.iter().any(|&i| {
        strokes[i].selection.is_none()
            && strokes[i].polygon.is_none()
            && strokes[i].coverage.is_none()
    });
    let mut clips = Vec::new();
    if !unrestricted {
        for (index, stroke) in strokes
            .iter()
            .enumerate()
            .filter(|(i, _)| active.contains(i))
        {
            if !clips.iter().any(|&i| {
                let existing: &Stroke = &strokes[i];
                existing.selection == stroke.selection && existing.polygon == stroke.polygon
            }) {
                clips.push(index);
            }
        }
        let cost = clips
            .iter()
            .map(|&i| strokes[i].polygon.as_ref().map_or(1, |p| p.len().max(1)))
            .sum::<usize>();
        if pixels * cost > MAX_SELECTION_WORK {
            return Err(
                "Liquify selection clipping exceeds the bounded work budget; split it into effects"
                    .into(),
            );
        }
    }
    let bytes = gw * gh * 8
        + pixels * 4
        + dabs.len() * std::mem::size_of::<Dab>()
        + changes * std::mem::size_of::<(usize, [f32; 2])>();
    if bytes > SCRATCH_BUDGET {
        return Err("Liquify exceeds the 256 MiB temporary working budget".into());
    }
    Ok(Plan {
        strokes,
        dabs,
        bounds,
        step,
        width: gw,
        height: gh,
        bytes,
        changes,
        clips,
        unrestricted,
    })
}
pub fn working_bytes(width: u32, height: u32, settings: &Value) -> Result<usize, String> {
    plan(width, height, settings).map(|p| p.bytes)
}

struct Field {
    values: Vec<[f32; 2]>,
    origin: [f32; 2],
    step: f32,
    width: usize,
    height: usize,
}
impl Field {
    fn sample(&self, p: [f32; 2]) -> [f32; 2] {
        let x = (p[0] - self.origin[0]) / self.step;
        let y = (p[1] - self.origin[1]) / self.step;
        let ix = x.floor() as i32;
        let iy = y.floor() as i32;
        let (fx, fy) = (x - ix as f32, y - iy as f32);
        let mut out = [0.0; 2];
        for (dx, dy, weight) in [
            (0, 0, (1.0 - fx) * (1.0 - fy)),
            (1, 0, fx * (1.0 - fy)),
            (0, 1, (1.0 - fx) * fy),
            (1, 1, fx * fy),
        ] {
            let (x, y) = (ix + dx, iy + dy);
            if x >= 0 && y >= 0 && (x as usize) < self.width && (y as usize) < self.height {
                let d = self.values[y as usize * self.width + x as usize];
                out[0] += d[0] * weight;
                out[1] += d[1] * weight;
            }
        }
        out
    }
}
fn sample(image: &Image, x: f32, y: f32) -> Pixel {
    let ix = x.floor() as i32;
    let iy = y.floor() as i32;
    let (fx, fy) = (x - ix as f32, y - iy as f32);
    let mut color = [0.0; 3];
    let mut alpha = 0.0;
    for (dx, dy, weight) in [
        (0, 0, (1.0 - fx) * (1.0 - fy)),
        (1, 0, fx * (1.0 - fy)),
        (0, 1, (1.0 - fx) * fy),
        (1, 1, fx * fy),
    ] {
        let pixel = image.get(ix + dx, iy + dy);
        let a = pixel[3] as f32 * weight;
        alpha += a;
        for c in 0..3 {
            color[c] += pixel[c] as f32 * a;
        }
    }
    if alpha < 0.00001 {
        return [0; 4];
    }
    [
        (color[0] / alpha).round().clamp(0.0, 255.0) as u8,
        (color[1] / alpha).round().clamp(0.0, 255.0) as u8,
        (color[2] / alpha).round().clamp(0.0, 255.0) as u8,
        alpha.round().clamp(0.0, 255.0) as u8,
    ]
}
pub fn apply(image: &mut Image, settings: &Value) -> Result<(), String> {
    let plan = plan(image.width, image.height, settings)?;
    if image.bytes.len() != image.width as usize * image.height as usize * 4 {
        return Err("Invalid liquify source pixels".into());
    }
    if plan.dabs.is_empty() {
        return Ok(());
    }
    let mapping = Mapping::from_plan(plan);
    let field = &mapping.field;
    let plan = &mapping.plan;
    let [left, top, right, bottom] = plan.bounds;
    let width = (right - left) as usize;
    let mut output = vec![0u8; width * (bottom - top) as usize * 4];
    for y in top..bottom {
        for x in left..right {
            let point = [x as f32 + 0.5, y as f32 + 0.5];
            let displacement = field.sample(point);
            let original = image.get(x, y);
            let pixel = if displacement[0].abs() + displacement[1].abs() < 0.000001
                || !(plan.unrestricted
                    || plan
                        .clips
                        .iter()
                        .any(|&index| plan.strokes[index].contains(point)))
            {
                original
            } else {
                sample(
                    image,
                    x as f32 + displacement[0],
                    y as f32 + displacement[1],
                )
            };
            let index = ((y - top) as usize * width + (x - left) as usize) * 4;
            output[index..index + 4].copy_from_slice(&pixel);
        }
    }
    for y in top..bottom {
        let start = (y as usize * image.width as usize + left as usize) * 4;
        let source = (y - top) as usize * width * 4;
        image.bytes[start..start + width * 4].copy_from_slice(&output[source..source + width * 4]);
    }
    Ok(())
}

/// Shared backward displacement mapping. Pixel precision is chosen by its sampler.
pub struct Mapping {
    plan: Plan,
    field: Field,
}
impl Mapping {
    fn from_plan(plan: Plan) -> Self {
        let mut field = Field {
            values: vec![[0.0; 2]; plan.width * plan.height],
            origin: [plan.bounds[0] as f32 + 0.5, plan.bounds[1] as f32 + 0.5],
            step: plan.step,
            width: plan.width,
            height: plan.height,
        };
        let mut changes = Vec::with_capacity(plan.changes);
        for dab in &plan.dabs {
            changes.clear();
            let stroke = &plan.strokes[dab.stroke];
            let r = stroke.radius;
            let to_grid = |n: f32, origin: f32| ((n - origin) / field.step).floor() as i32;
            let left = to_grid(dab.center[0] - r, field.origin[0]).max(0);
            let top = to_grid(dab.center[1] - r, field.origin[1]).max(0);
            let right =
                (to_grid(dab.center[0] + r, field.origin[0]) + 1).min(field.width as i32 - 1);
            let bottom =
                (to_grid(dab.center[1] + r, field.origin[1]) + 1).min(field.height as i32 - 1);
            for y in top..=bottom {
                for x in left..=right {
                    let point = [
                        field.origin[0] + x as f32 * field.step,
                        field.origin[1] + y as f32 * field.step,
                    ];
                    let offset = [point[0] - dab.center[0], point[1] - dab.center[1]];
                    let q = (offset[0] * offset[0] + offset[1] * offset[1]) / (r * r);
                    if q >= 1.0 || !stroke.contains(point) {
                        continue;
                    }
                    let falloff = (1.0 - q)
                        * (1.0 - q)
                        * dab.strength
                        * stroke.coverage.as_ref().map_or(1.0, |m| {
                            m.value(point[0].floor() as i32, point[1].floor() as i32)
                        });
                    let index = y as usize * field.width + x as usize;
                    let displacement = if matches!(stroke.mode, Mode::Restore) {
                        let old = field.values[index];
                        [old[0] * (1.0 - falloff), old[1] * (1.0 - falloff)]
                    } else {
                        let delta = match stroke.mode {
                            Mode::Push => [-dab.delta[0] * falloff, -dab.delta[1] * falloff],
                            Mode::Expand => {
                                [-offset[0] * falloff * 0.15, -offset[1] * falloff * 0.15]
                            }
                            Mode::Pinch => [offset[0] * falloff * 0.15, offset[1] * falloff * 0.15],
                            Mode::Restore => unreachable!(),
                        };
                        let inverse = [point[0] + delta[0], point[1] + delta[1]];
                        if !stroke.contains(inverse) {
                            continue;
                        }
                        let old = field.sample(inverse);
                        [delta[0] + old[0], delta[1] + old[1]]
                    };
                    changes.push((index, displacement));
                }
            }
            for &(index, displacement) in &changes {
                field.values[index] = displacement;
            }
        }
        Self { plan, field }
    }
    pub fn bounds(&self) -> [i32; 4] {
        self.plan.bounds
    }
    pub fn source_position(&self, x: i32, y: i32) -> Option<[f32; 2]> {
        let point = [x as f32 + 0.5, y as f32 + 0.5];
        let displacement = self.field.sample(point);
        if displacement[0].abs() + displacement[1].abs() < 0.000001
            || !(self.plan.unrestricted
                || self
                    .plan
                    .clips
                    .iter()
                    .any(|&i| self.plan.strokes[i].contains(point)))
        {
            None
        } else {
            Some([x as f32 + displacement[0], y as f32 + displacement[1]])
        }
    }
}
fn native_plan(width: u32, height: u32, settings: &Value) -> Result<Plan, String> {
    let mut plan = plan(width, height, settings)?;
    let [l, t, r, b] = plan.bounds;
    plan.bytes += (r - l) as usize * (b - t) as usize * 4;
    if plan.bytes > SCRATCH_BUDGET {
        return Err("Native16 liquify exceeds the256 MiB working budget".into());
    }
    Ok(plan)
}
pub fn working_bytes16(width: u32, height: u32, settings: &Value) -> Result<usize, String> {
    native_plan(width, height, settings).map(|p| p.bytes)
}
pub fn mapping16(width: u32, height: u32, settings: &Value) -> Result<Mapping, String> {
    native_plan(width, height, settings).map(Mapping::from_plan)
}
