//! Shared rectangle, ellipse and alpha-gradient raster math.
use crate::raster::{project16, Pixel, Pixel16};
use serde_json::Value;

#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub(crate) enum GradientDirection {
    #[default]
    Right,
    Left,
    Down,
    Up,
}

pub(crate) fn gradient_color(mut color: Pixel, position: f32) -> Pixel {
    color[3] = (f32::from(color[3]) * (1. - position)) as u8;
    color
}

impl GradientDirection {
    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::Right => "right",
            Self::Left => "left",
            Self::Down => "down",
            Self::Up => "up",
        }
    }

    fn distance(self, [left, top, right, bottom]: [i32; 4], x: i32, y: i32) -> (i32, i32) {
        match self {
            Self::Right => (x - left, right - left),
            Self::Left => (right - 1 - x, right - left),
            Self::Down => (y - top, bottom - top),
            Self::Up => (bottom - 1 - y, bottom - top),
        }
    }
}

pub(crate) struct Primitive {
    area: [i32; 4],
    color8: Pixel,
    color16: Pixel16,
    ellipse: bool,
    gradient: Option<GradientDirection>,
    adjustment: Option<[f64; 3]>,
}
impl Primitive {
    pub(crate) fn new(command: &Value, area: [i32; 4], depth: u16) -> Result<Self, String> {
        let color = super::raster_color(command, depth)?;
        let gradient = if command["op"] == "gradient" {
            Some(match command.get("direction") {
                None => GradientDirection::Right,
                Some(value) => match value.as_str() {
                    Some("right") => GradientDirection::Right,
                    Some("left") => GradientDirection::Left,
                    Some("down") => GradientDirection::Down,
                    Some("up") => GradientDirection::Up,
                    _ => return Err("Choose right, left, down or up for the gradient direction".into()),
                },
            })
        } else {
            None
        };
        Ok(Self {
            area,
            color8: color.map(project16),
            color16: color,
            ellipse: command["op"] == "shape" && command["kind"] == "ellipse",
            gradient,
            adjustment: (command["op"] == "adjust").then(|| {
                [
                    super::num(command, "brightness", 0.).clamp(-1., 1.),
                    super::num(command, "contrast", 1.).clamp(0., 3.),
                    super::num(command, "saturation", 1.).clamp(0., 3.),
                ]
            }),
        })
    }
    pub(crate) fn contains(&self, x: i32, y: i32) -> bool {
        if !self.ellipse {
            return true;
        }
        let rx = (self.area[2] - self.area[0]) as f32 / 2.0;
        let ry = (self.area[3] - self.area[1]) as f32 / 2.0;
        rx > 0.0
            && ry > 0.0
            && ((x as f32 - self.area[0] as f32 - rx) / rx).powi(2)
                + ((y as f32 - self.area[1] as f32 - ry) / ry).powi(2)
                <= 1.0
    }
    pub(crate) fn blend8(&self, x: i32, y: i32, before: [u8; 4]) -> [u8; 4] {
        if let Some(settings) = self.adjustment {
            if before == [0; 4] {
                return before;
            }
            let [brightness, contrast, saturation] = settings.map(|v| v as f32);
            if [brightness, contrast, saturation] == [0., 1., 1.] {
                return before;
            }
            let mut after = before;
            let gray =
                before[0] as f32 * 0.2126 + before[1] as f32 * 0.7152 + before[2] as f32 * 0.0722;
            for c in 0..3 {
                after[c] = ((gray + (before[c] as f32 - gray) * saturation - 127.5) * contrast
                    + 127.5
                    + brightness * 255.)
                    .clamp(0., 255.) as u8;
            }
            return after;
        }
        let mut color = self.color8;
        if let Some(direction) = self.gradient {
            let (distance, length) = direction.distance(self.area, x, y);
            let t = distance as f32 / length.max(1) as f32;
            color = gradient_color(color, t);
        }
        crate::raster::blend(before, color, 1.0, "normal")
    }
    pub(crate) fn blend16(&self, x: i32, y: i32, before: [u16; 4]) -> [u16; 4] {
        if let Some([brightness, contrast, saturation]) = self.adjustment {
            if before == [0; 4] {
                return before;
            }
            if [brightness, contrast, saturation] == [0., 1., 1.] {
                return before;
            }
            let mut after = before;
            let gray = f64::from(before[0]) * 0.2126
                + f64::from(before[1]) * 0.7152
                + f64::from(before[2]) * 0.0722;
            for c in 0..3 {
                after[c] = ((gray + (f64::from(before[c]) - gray) * saturation - 32767.5)
                    * contrast
                    + 32767.5
                    + brightness * 65535.)
                    .clamp(0., 65535.)
                    .round() as u16;
            }
            return after;
        }
        let mut color = self.color16;
        if let Some(direction) = self.gradient {
            let (distance, length) = direction.distance(self.area, x, y);
            let t = f64::from(distance) / f64::from(length.max(1));
            color[3] = (f64::from(color[3]) * (1.0 - t)).round() as u16;
        }
        crate::raster::blend16(before, color, 1.0, "normal")
    }
}
