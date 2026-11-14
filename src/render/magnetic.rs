//! Magnetic-lasso geometry from a bounded shared-renderer neighborhood.
use crate::{
    engine::{scene::SceneSnapshot, Document},
    raster::Pixel,
    workspace::source_runtime::SourceReadSession,
};
use std::sync::Arc;

pub(crate) fn area(size: [u32; 2], point: [f32; 2], width: f32) -> Result<[i32; 4], String> {
    let [x, y] = super::sample::point(size, point)?;
    if !width.is_finite() {
        return Err("Magnetic snap width must be finite".into());
    }
    let [w, h] = size.map(i32::try_from);
    let (w, h) = (
        w.map_err(|_| "Magnetic canvas is too wide")?,
        h.map_err(|_| "Magnetic canvas is too tall")?,
    );
    let radius = width.clamp(2., 32.) as i32;
    Ok([
        x.saturating_sub(radius).max(0),
        y.saturating_sub(radius).max(0),
        x.saturating_add(radius + 1).min(w),
        y.saturating_add(radius + 1).min(h),
    ])
}

fn snap(
    area: [i32; 4],
    point: [f32; 2],
    mut pixel: impl FnMut(u32, u32) -> Result<Pixel, String>,
    check: &mut dyn FnMut() -> Result<(), String>,
) -> Result<[f32; 2], String> {
    let (w, h) = ((area[2] - area[0]) as u32, (area[3] - area[1]) as u32);
    let mut luminance = |x, y| {
        pixel(x, y).map(|p| 0.2126 * p[0] as f32 + 0.7152 * p[1] as f32 + 0.0722 * p[2] as f32)
    };
    let mut best = point;
    let mut score = 8.;
    for y in 1..h.saturating_sub(1) {
        check()?;
        for x in 1..w.saturating_sub(1) {
            let dx = area[0] as f32 + x as f32 - point[0];
            let dy = area[1] as f32 + y as f32 - point[1];
            let edge = ((luminance(x + 1, y)? - luminance(x - 1, y)?).powi(2)
                + (luminance(x, y + 1)? - luminance(x, y - 1)?).powi(2))
            .sqrt();
            let value = edge / (1. + 0.2 * (dx * dx + dy * dy).sqrt());
            if value > score {
                score = value;
                best = [area[0] as f32 + x as f32, area[1] as f32 + y as f32];
            }
        }
    }
    check()?;
    Ok(best)
}

pub(crate) fn resident(
    doc: &Document,
    point: [f32; 2],
    width: f32,
    check: &mut dyn FnMut() -> Result<(), String>,
) -> Result<[f32; 2], String> {
    check()?;
    let area = area([doc.width, doc.height], point, width)?;
    let (w, h, bytes, rendered) = doc.preview(Some(area), 65, None, false)?;
    if rendered != area || w != (area[2] - area[0]) as u32 || h != (area[3] - area[1]) as u32 {
        return Err("Magnetic renderer returned another document region".into());
    }
    snap(
        area,
        point,
        |x, y| {
            let offset = ((y * w + x) * 4) as usize;
            bytes
                .get(offset..offset + 4)
                .and_then(|p| p.try_into().ok())
                .ok_or_else(|| "Magnetic renderer returned incomplete RGBA".into())
        },
        check,
    )
}

pub(crate) fn native(
    scene: Arc<SceneSnapshot>,
    session: impl Into<SourceReadSession>,
    point: [f32; 2],
    width: f32,
    check: &mut dyn FnMut() -> Result<(), String>,
) -> Result<[f32; 2], String> {
    check()?;
    let doc = scene.metadata();
    let area = area([doc.width(), doc.height()], point, width)?;
    let request =
        super::native_preview::Request::capture(scene, session, Some(area), 65, None, false)?;
    let frame = request.render(check)?;
    let key = frame.key();
    if key.rect != area
        || key.width != (area[2] - area[0]) as u32
        || key.height != (area[3] - area[1]) as u32
    {
        return Err("Magnetic renderer returned another document region".into());
    }
    snap(
        area,
        point,
        |x, y| {
            let stripe = frame
                .stripes()
                .iter()
                .find(|stripe| {
                    let rows = stripe.rows();
                    y >= rows[0] && y < rows[1]
                })
                .ok_or("Magnetic renderer omitted a row")?;
            let offset = (((y - stripe.rows()[0]) * key.width + x) * 4) as usize;
            stripe
                .bytes()
                .get(offset..offset + 4)
                .and_then(|p| p.try_into().ok())
                .ok_or_else(|| "Magnetic renderer returned incomplete RGBA".into())
        },
        check,
    )
}

#[cfg(test)]
#[path = "magnetic_tests.rs"]
mod tests;
