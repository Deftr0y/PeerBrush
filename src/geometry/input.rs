//! Shared canvas/image dimensions, crop coordinates and anchor policy.
use crate::raster::CanvasLimits;
use serde_json::Value;

pub(crate) fn scaled_size(
    size: [u32; 2],
    scale: [f32; 2],
    limits: CanvasLimits,
) -> Result<[u32; 2], String> {
    let output =
        std::array::from_fn(|i| (f64::from(size[i]) * f64::from(scale[i])).round().max(1.) as u32);
    limits.check(output[0], output[1])?;
    Ok(output)
}
pub(crate) fn coordinate(at: u32, source: u32, target: u32) -> f32 {
    ((at as f32 + 0.5) * (source as f32 / target as f32) - 0.5).clamp(0., source as f32 - 1.)
}
pub(crate) fn coverage_coordinate(at: u32, source: u32, target: u32) -> f64 {
    ((f64::from(at) + 0.5) * f64::from(source) / f64::from(target) - 0.5)
        .clamp(0., f64::from(source) - 1.)
}
pub(crate) fn coverage<E>(
    x: u32,
    y: u32,
    source: [u32; 2],
    target: [u32; 2],
    mut get: impl FnMut(i32, i32) -> Result<u8, E>,
) -> Result<u8, E> {
    let fx = coverage_coordinate(x, source[0], target[0]);
    let fy = coverage_coordinate(y, source[1], target[1]);
    let (ix, iy) = (fx.floor() as i32, fy.floor() as i32);
    let (rx, ry) = (fx - fx.floor(), fy - fy.floor());
    let mut sum = 0.;
    for (dx, dy, weight) in [
        (0, 0, (1. - rx) * (1. - ry)),
        (1, 0, rx * (1. - ry)),
        (0, 1, (1. - rx) * ry),
        (1, 1, rx * ry),
    ] {
        sum += f64::from(get(
            (ix + dx).min(source[0] as i32 - 1),
            (iy + dy).min(source[1] as i32 - 1),
        )?) * weight;
    }
    Ok(sum.round() as u8)
}
#[derive(Clone, Copy)]
pub(crate) struct Frame {
    pub width: u32,
    pub height: u32,
    pub offset: [i32; 2],
    pub scale: [f32; 2],
    pub image: bool,
    pub proportional: bool,
}
fn dimension(c: &Value, key: &str, edge: u32) -> Result<u32, String> {
    c[key]
        .as_u64()
        .filter(|v| *v <= u64::from(edge))
        .map(|v| v as u32)
        .ok_or_else(|| format!("{key} must be an integer from 1 to {edge}"))
}
impl Frame {
    pub(crate) fn origin(&self, origin: [i32; 2]) -> Result<[i32; 2], String> {
        let point =
            std::array::from_fn::<_, 2, _>(|i| f64::from(origin[i]) * f64::from(self.scale[i]));
        if (0..2).any(|i| (point[i] + f64::from(self.offset[i])).abs() > 100000.) {
            return Err("Layer geometry would exceed coordinate limits".into());
        }
        Ok(std::array::from_fn(|i| {
            point[i].round() as i32 + self.offset[i]
        }))
    }
    pub(crate) fn new(canvas: [u32; 2], c: &Value, limits: CanvasLimits) -> Result<Self, String> {
        let image = matches!(c["op"].as_str(), Some("image.resize" | "resize"));
        let (width, height, offset) = if c["op"] == "crop" {
            let b = c["rect"]
                .as_array()
                .filter(|b| b.len() == 4)
                .ok_or("Crop needs [left,top,right,bottom]")?;
            let mut r = [0; 4];
            for (i, v) in b.iter().enumerate() {
                r[i] = v
                    .as_i64()
                    .filter(|v| (-100000..=100000).contains(v))
                    .ok_or("Crop coordinates must be bounded integers")?
                    as i32;
            }
            if r[0] >= r[2] || r[1] >= r[3] {
                return Err("Crop needs a positive rectangle".into());
            }
            ((r[2] - r[0]) as u32, (r[3] - r[1]) as u32, [-r[0], -r[1]])
        } else {
            let (w, h) = (
                dimension(c, "width", limits.edge())?,
                dimension(c, "height", limits.edge())?,
            );
            let offset = if image {
                [0, 0]
            } else {
                let anchor = c.get("anchor").and_then(Value::as_str).unwrap_or("center");
                let (ax, ay) = match anchor {
                    "top_left" => (0., 0.),
                    "top" => (0.5, 0.),
                    "top_right" => (1., 0.),
                    "left" => (0., 0.5),
                    "center" => (0.5, 0.5),
                    "right" => (1., 0.5),
                    "bottom_left" => (0., 1.),
                    "bottom" => (0.5, 1.),
                    "bottom_right" => (1., 1.),
                    _ => return Err("Choose one of the nine canvas anchors".into()),
                };
                if c.get("anchor").is_some_and(|v| !v.is_string()) {
                    return Err("Canvas anchor must be a name".into());
                }
                [
                    ((w as f64 - canvas[0] as f64) * ax).round() as i32,
                    ((h as f64 - canvas[1] as f64) * ay).round() as i32,
                ]
            };
            (w, h, offset)
        };
        limits.check(width, height)?;
        let scale = if image {
            [
                width as f32 / canvas[0] as f32,
                height as f32 / canvas[1] as f32,
            ]
        } else {
            [1., 1.]
        };
        let proportional = !image
            || (height as f64 - width as f64 * canvas[1] as f64 / canvas[0] as f64).abs()
                <= 0.500001
            || (width as f64 - height as f64 * canvas[0] as f64 / canvas[1] as f64).abs()
                <= 0.500001;
        Ok(Self {
            width,
            height,
            offset,
            scale,
            image,
            proportional,
        })
    }
}
