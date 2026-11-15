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
    let _ = apply_region_checked(doc, width, height, bytes, rect, mode, &|| Ok(()));
}

pub(crate) fn apply_region_checked(
    doc: &Document,
    width: u32,
    height: u32,
    bytes: &mut [u8],
    rect: [i32; 4],
    mode: &str,
    check: &crate::render::Check<'_>,
) -> Result<(), String> {
    check()?;
    let Some(bounds) = doc.selection else {
        return Ok(());
    };
    let coverage = doc.selection_coverage.as_ref().filter(|coverage| coverage.bounds == bounds);
    let polygon = coverage.is_none().then(|| super::polygon(doc)).flatten();
    let [left, top, right, bottom] = [bounds[0].max(0), bounds[1].max(0),
        bounds[2].min(doc.width as i32), bounds[3].min(doc.height as i32)];
    // Only the longest source edge retains the exact preview scale after rounding.
    let (edge, source_edge) = if rect[2] - rect[0] >= rect[3] - rect[1] {
        (width, rect[2] - rect[0])
    } else { (height, rect[3] - rect[1]) };
    let scale8 = edge as f32 / source_edge as f32;
    let scale16 = edge as f64 / source_edge as f64;
    let coordinate = |value: u32| {
        if doc.bit_depth == 16 { (value as f64 / scale16) as i32 }
        else { (value as f32 / scale8) as i32 }
    };
    for y in 0..height {
        check()?;
        let sy = rect[1] + coordinate(y);
        for x in 0..width {
            let sx = rect[0] + coordinate(x);
            let value = if let Some(coverage) = coverage {
                coverage.value(sx, sy)
            } else if sx >= left && sx < right && sy >= top && sy < bottom
                && polygon.is_none_or(|points| super::contains(points, sx as f32 + 0.5, sy as f32 + 0.5))
            {
                1.
            } else {
                0.
            };
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
    check()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn geometry_projects_like_materialized_coverage_on_full_and_reduced_grids() {
        let mut doc = Document::new_depth(9, 5, 16).unwrap();
        let triangle = vec![[-2., 0.], [8., 0.], [1., 4.]];
        doc.selection = Some(crate::selection::bounds(&triangle).unwrap());
        doc.selection_polygon = Some(triangle);
        let mut rasterized = doc.clone();
        rasterized.selection_coverage = crate::selection::current(&doc);
        for (width, height) in [(9, 5), (4, 2)] {
            let mut actual = vec![255; (width * height * 4) as usize];
            let mut expected = actual.clone();
            apply(&doc, width, height, &mut actual, "cutout");
            apply(&rasterized, width, height, &mut expected, "cutout");
            assert_eq!(actual, expected);
            assert!(actual.chunks_exact(4).any(|pixel| pixel[3] == 0));
            assert!(actual.chunks_exact(4).any(|pixel| pixel[3] == 255));
        }
    }

    #[test]
    fn obsolete_selection_projection_stops_before_processing_the_next_row() {
        let mut doc = Document::new(4, 3).unwrap();
        doc.selection = Some([0, 0, 2, 3]);
        let mut pixels = [255; 4 * 3 * 4];
        let calls = AtomicUsize::new(0);
        let result = apply_region_checked(&doc, 4, 3, &mut pixels, [0, 0, 4, 3], "cutout", &|| {
            if calls.fetch_add(1, Ordering::Relaxed) == 2 {
                Err("Preview superseded".into())
            } else {
                Ok(())
            }
        });
        assert_eq!(result.unwrap_err(), "Preview superseded");
        assert_eq!(pixels[11], 0);
        assert_eq!(&pixels[16..], &[255; 32]);
    }
}
