//! Presentation-only selection coverage; never written to document sources.
use crate::{engine::Document, raster::blend};
pub fn apply(doc: &Document, width: u32, height: u32, bytes: &mut [u8], mode: &str) {
    apply_region(
        doc,
        width,
        height,
        bytes,
        [0, 0, doc.width as i32, doc.height as i32],
        mode,
    );
}
pub fn apply_region(
    doc: &Document,
    width: u32,
    height: u32,
    bytes: &mut [u8],
    rect: [i32; 4],
    mode: &str,
) {
    let Some(coverage) = super::current(doc) else {
        return;
    };
    let scale =
        (width as f32 / (rect[2] - rect[0]) as f32).min(height as f32 / (rect[3] - rect[1]) as f32);
    for y in 0..height {
        for x in 0..width {
            let value = coverage
                .value(
                    rect[0] + (x as f32 / scale) as i32,
                    rect[1] + (y as f32 / scale) as i32,
                )
                .clamp(0., 1.);
            let at = ((y * width + x) * 4) as usize;
            let mut p = [bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]];
            match mode {
                "mask" => {
                    let v = (value * 255.).round() as u8;
                    p = [v, v, v, 255];
                }
                "overlay" => {
                    p = blend(
                        p,
                        [233, 84, 32, ((1. - value) * 140.).round() as u8],
                        1.,
                        "normal",
                    );
                }
                _ => p[3] = (p[3] as f32 * value).round() as u8,
            }
            bytes[at..at + 4].copy_from_slice(&p);
        }
    }
}
