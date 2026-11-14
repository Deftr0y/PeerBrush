//! Shared precision-preserving premultiplied bilinear sampling.
use super::{Pixel, Pixel16};
pub(crate) fn sample16<E>(
    x: f32,
    y: f32,
    mut get: impl FnMut(i32, i32) -> Result<Pixel16, E>,
) -> Result<Pixel16, E> {
    if x.fract() == 0.0 && y.fract() == 0.0 {
        return get(x as i32, y as i32);
    }
    let ix = x.floor() as i32;
    let iy = y.floor() as i32;
    let fx = (x - x.floor()) as f64;
    let fy = (y - y.floor()) as f64;
    let mut sum = [0.0f64; 4];
    for (dx, dy, w) in [
        (0, 0, (1. - fx) * (1. - fy)),
        (1, 0, fx * (1. - fy)),
        (0, 1, (1. - fx) * fy),
        (1, 1, fx * fy),
    ] {
        let p = get(ix + dx, iy + dy)?;
        let a = p[3] as f64 / 65535.;
        for c in 0..3 {
            sum[c] += p[c] as f64 * a * w;
        }
        sum[3] += a * w;
    }
    if sum[3] <= 0. {
        return Ok([0; 4]);
    }
    Ok([
        (sum[0] / sum[3]).round().clamp(0., 65535.) as u16,
        (sum[1] / sum[3]).round().clamp(0., 65535.) as u16,
        (sum[2] / sum[3]).round().clamp(0., 65535.) as u16,
        (sum[3] * 65535.).round().clamp(0., 65535.) as u16,
    ])
}
pub(crate) fn sample<E>(
    x: f32,
    y: f32,
    mut get: impl FnMut(i32, i32) -> Result<Pixel, E>,
) -> Result<Pixel, E> {
    if x.fract() == 0.0 && y.fract() == 0.0 {
        return get(x as i32, y as i32);
    }
    let ix = x.floor() as i32;
    let iy = y.floor() as i32;
    let (fx, fy) = (x - x.floor(), y - y.floor());
    let mut out = [0.0f32; 4];
    for (dx, dy, weight) in [
        (0, 0, (1.0 - fx) * (1.0 - fy)),
        (1, 0, fx * (1.0 - fy)),
        (0, 1, (1.0 - fx) * fy),
        (1, 1, fx * fy),
    ] {
        let p = get(ix + dx, iy + dy)?;
        let alpha = p[3] as f32 / 255.0;
        for c in 0..3 {
            out[c] += p[c] as f32 * alpha * weight;
        }
        out[3] += alpha * weight;
    }
    if out[3] <= 0.00001 {
        return Ok([0; 4]);
    }
    Ok([
        (out[0] / out[3]).round() as u8,
        (out[1] / out[3]).round() as u8,
        (out[2] / out[3]).round() as u8,
        (out[3] * 255.0).round() as u8,
    ])
}
