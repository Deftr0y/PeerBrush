//! Bounded native contour curves, prepared once per effect render.
use serde_json::Value;

pub fn contains(kind: &str) -> bool {
    matches!(
        kind,
        "contour"
            | "bevel_emboss"
            | "inner_shadow"
            | "inner_glow"
            | "satin"
            | "outer_glow"
            | "drop_shadow"
    )
}
pub fn extended(kind: &str, settings: &Value) -> bool {
    contains(kind) && settings.get("contour_points").is_some()
}
pub struct Curve(Vec<[f64; 2]>);
impl Curve {
    pub fn decode(value: &Value) -> Result<Self, String> {
        let points = value
            .as_array()
            .filter(|v| (2..=16).contains(&v.len()))
            .ok_or("Custom contour requires 2–16 points")?;
        let mut result = Vec::with_capacity(points.len());
        let mut previous = -1.;
        for (index, point) in points.iter().enumerate() {
            let fields = point.as_object().ok_or("Contour point must be an object")?;
            if fields
                .keys()
                .any(|key| !matches!(key.as_str(), "position" | "value"))
            {
                return Err("Unsupported contour point property".into());
            }
            let number = |key: &str| {
                point[key]
                    .as_f64()
                    .filter(|n| n.is_finite() && (0.0..=1.).contains(n))
                    .ok_or("Contour point position/value must be finite within 0–1")
            };
            let position = number("position")?;
            if position <= previous
                || index == 0 && position != 0.
                || index + 1 == points.len() && position != 1.
            {
                return Err("Contour positions must strictly increase from 0 to 1".into());
            }
            result.push([position, number("value")?]);
            previous = position;
        }
        Ok(Self(result))
    }
    pub fn sample(&self, position: f64) -> f64 {
        let position = if position.is_nan() {
            0.
        } else {
            position.clamp(0., 1.)
        };
        let index = self
            .0
            .partition_point(|point| point[0] < position)
            .saturating_sub(1)
            .min(self.0.len() - 2);
        let (a, b) = (self.0[index], self.0[index + 1]);
        let t = (position - a[0]) / (b[0] - a[0]);
        (a[1] * (1. - t) + b[1] * t).clamp(0., 1.)
    }
}
