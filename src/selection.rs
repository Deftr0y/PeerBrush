//! Document-space selection geometry. The public rectangle stays its bounding box.
use crate::engine::Document;
mod system;
pub use system::*;

pub fn rectangle(area: [i32; 4]) -> Vec<[f32; 2]> {
    vec![
        [area[0] as f32, area[1] as f32],
        [area[2] as f32, area[1] as f32],
        [area[2] as f32, area[3] as f32],
        [area[0] as f32, area[3] as f32],
    ]
}

pub fn bounds(points: &[[f32; 2]]) -> Result<[i32; 4], String> {
    if !(3..=8192).contains(&points.len())
        || points
            .iter()
            .flatten()
            .any(|p| !p.is_finite() || p.abs() > 100000.0)
    {
        return Err(
            "Selection needs 3–8192 finite points within document coordinate limits".into(),
        );
    }
    let mut bounds = [
        f32::INFINITY,
        f32::INFINITY,
        f32::NEG_INFINITY,
        f32::NEG_INFINITY,
    ];
    let mut area = 0.0f64;
    for (index, p) in points.iter().enumerate() {
        bounds = [
            bounds[0].min(p[0]),
            bounds[1].min(p[1]),
            bounds[2].max(p[0]),
            bounds[3].max(p[1]),
        ];
        let q = points[(index + 1) % points.len()];
        area += p[0] as f64 * q[1] as f64 - q[0] as f64 * p[1] as f64;
    }
    if area.abs() < 0.000001 || bounds[0] >= bounds[2] || bounds[1] >= bounds[3] {
        return Err("Selection must have positive area".into());
    }
    Ok([
        bounds[0].floor() as i32,
        bounds[1].floor() as i32,
        bounds[2].ceil() as i32,
        bounds[3].ceil() as i32,
    ])
}

/// Pixel centers on an edge belong to the selection, keeping transformed corners stable.
pub fn contains(points: &[[f32; 2]], x: f32, y: f32) -> bool {
    if points.len() < 3 || !x.is_finite() || !y.is_finite() {
        return false;
    }
    let mut inside = false;
    let mut a = points[points.len() - 1];
    for &b in points {
        let dx = b[0] - a[0];
        let dy = b[1] - a[1];
        let cross = (x - a[0]) * dy - (y - a[1]) * dx;
        if cross.abs() <= 0.00001 * (dx.abs() + dy.abs()).max(1.0)
            && x >= a[0].min(b[0])
            && x <= a[0].max(b[0])
            && y >= a[1].min(b[1])
            && y <= a[1].max(b[1])
        {
            return true;
        }
        if (a[1] > y) != (b[1] > y) && x < (b[0] - a[0]) * (y - a[1]) / (b[1] - a[1]) + a[0] {
            inside = !inside;
        }
        a = b;
    }
    inside
}

pub fn polygon(doc: &Document) -> Option<&[[f32; 2]]> {
    // Direct legacy callers may replace the rectangle; an unrelated old polygon must not clip it.
    let rectangle = doc.selection?;
    let polygon = doc.selection_polygon.as_deref()?;
    (bounds(polygon).ok() == Some(rectangle)).then_some(polygon)
}

pub fn contains_pixel(doc: &Document, x: i32, y: i32) -> bool {
    if let Some(coverage) = &doc.selection_coverage {
        if doc.selection == Some(coverage.bounds) {
            return coverage.value(x, y) > 0.0;
        }
    }
    let Some(area) = doc.selection else {
        return true;
    };
    x >= area[0]
        && y >= area[1]
        && x < area[2]
        && y < area[3]
        && polygon(doc).is_none_or(|points| contains(points, x as f32 + 0.5, y as f32 + 0.5))
}
