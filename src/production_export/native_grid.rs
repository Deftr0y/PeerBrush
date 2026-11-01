//! Exact export sample coordinates, partitioned into bounded preparation windows.
//! This owns no pixels and grants no source or destination authority.
use super::{Output, Resampling};

type Result<T> = std::result::Result<T, String>;
const EDGE: i32 = 256;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Window {
    /// Half-open coordinates in the final output, independent of stripe origin.
    pub output: [u32; 4],
    /// Includes the precise nearest/bilinear support, including padded exterior.
    pub source: [i32; 4],
}

pub(crate) struct Grid {
    rect: [i32; 4],
    size: [u32; 2],
    method: Resampling,
    // Depth-first subdivision needs at most log2(width)+log2(height)+1 entries.
    pending: [[u32; 4]; 64],
    count: usize,
}
impl Grid {
    pub(crate) fn new(
        output: &Output,
        method: Resampling,
        first_row: u32,
        rows: u32,
    ) -> Result<Self> {
        let rect = output.document_rect;
        let last = first_row.checked_add(rows).ok_or("Export row overflow")?;
        if output.width == 0
            || output.height == 0
            || output.width > 16000
            || output.height > 16000
            || rows == 0
            || last > output.height
            || u64::from(output.width) * u64::from(rows) > 128 * 1024
            || rect[0] >= rect[2]
            || rect[1] >= rect[3]
            || rect.iter().any(|v| v.unsigned_abs() > 116000)
        {
            return Err("Invalid bounded native export grid".into());
        }
        let mut grid = Self {
            rect,
            size: [output.width, output.height],
            method,
            pending: [[0; 4]; 64],
            count: 1,
        };
        grid.pending[0] = [0, first_row, output.width, last];
        Ok(grid)
    }
    pub(crate) fn position(&self, x: u32, y: u32) -> Result<[f64; 2]> {
        if x >= self.size[0] || y >= self.size[1] {
            return Err("Export sample is outside the planned output".into());
        }
        Ok([self.axis(x, 0), self.axis(y, 1)])
    }
    fn axis(&self, value: u32, axis: usize) -> f64 {
        // Keep the legacy order of f64 operations and clamp before sampling.
        (f64::from(self.rect[axis])
            + (f64::from(value) + 0.5) * f64::from(self.rect[axis + 2] - self.rect[axis])
                / f64::from(self.size[axis])
            - 0.5)
            .clamp(
                f64::from(self.rect[axis]),
                f64::from(self.rect[axis + 2] - 1),
            )
    }
    fn support(&self, bounds: [u32; 4]) -> [i32; 4] {
        let mut support = [0; 4];
        for axis in 0..2 {
            let first = self.axis(bounds[axis], axis);
            let last = self.axis(bounds[axis + 2] - 1, axis);
            if self.method == Resampling::Nearest {
                support[axis] = (first + 0.5).floor() as i32;
                support[axis + 2] = (last + 0.5).floor() as i32 + 1;
            } else {
                support[axis] = first.floor() as i32;
                support[axis + 2] = last.floor() as i32 + 2;
            }
        }
        support
    }
    pub(crate) fn next(&mut self) -> Result<Option<Window>> {
        while self.count != 0 {
            self.count -= 1;
            let bounds = self.pending[self.count];
            let source = self.support(bounds);
            let widths = [source[2] - source[0], source[3] - source[1]];
            if widths[0] <= EDGE && widths[1] <= EDGE {
                return Ok(Some(Window {
                    output: bounds,
                    source,
                }));
            }
            let axis = if widths[0] > EDGE { 0 } else { 1 };
            let middle = bounds[axis] + (bounds[axis + 2] - bounds[axis]) / 2;
            if middle == bounds[axis] || self.count + 2 > self.pending.len() {
                return Err("Native export grid subdivision is inconsistent".into());
            }
            let mut left = bounds;
            let mut right = bounds;
            left[axis + 2] = middle;
            right[axis] = middle;
            self.pending[self.count] = right;
            self.pending[self.count + 1] = left;
            self.count += 2;
        }
        Ok(None)
    }
}

/// Sample already prepared pixels. The callback must refuse missing support;
/// preparation, cancellation and native precision remain the caller's duty.
pub(crate) fn sample(
    position: [f64; 2],
    method: Resampling,
    mut pixel: impl FnMut(i32, i32) -> Result<[u16; 4]>,
) -> Result<[u16; 4]> {
    let [x, y] = position;
    if !x.is_finite() || !y.is_finite() || x.abs() > 116000. || y.abs() > 116000. {
        return Err("Invalid native export sample position".into());
    }
    if method == Resampling::Nearest {
        return pixel((x + 0.5).floor() as i32, (y + 0.5).floor() as i32);
    }
    let (ix, iy) = (x.floor() as i32, y.floor() as i32);
    let (fx, fy) = (x - f64::from(ix), y - f64::from(iy));
    if fx == 0. && fy == 0. {
        return pixel(ix, iy);
    }
    let weights = [
        (1. - fx) * (1. - fy),
        fx * (1. - fy),
        (1. - fx) * fy,
        fx * fy,
    ];
    let pixels = [
        pixel(ix, iy)?,
        pixel(ix + 1, iy)?,
        pixel(ix, iy + 1)?,
        pixel(ix + 1, iy + 1)?,
    ];
    let alpha: f64 = (0..4).map(|i| weights[i] * f64::from(pixels[i][3])).sum();
    if alpha == 0. {
        return Ok([0; 4]);
    }
    let mut out = [0; 4];
    out[3] = alpha.round() as u16;
    for (c, value) in out.iter_mut().take(3).enumerate() {
        *value = ((0..4)
            .map(|i| weights[i] * f64::from(pixels[i][c]) * f64::from(pixels[i][3]))
            .sum::<f64>()
            / alpha)
            .round() as u16;
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::production_export::{Format, Image, Pixels};

    fn output(width: u32, height: u32, rect: [i32; 4]) -> Output {
        Output {
            target: None,
            target_name: String::new(),
            variant: String::new(),
            filename: "test.png".into(),
            document_rect: rect,
            width,
            height,
            format: Format::Png,
            native_source_depth: 16,
            bit_depth: 16,
            scale_xy: [1., 1.],
        }
    }
    #[test]
    fn partition_covers_exact_stripe_and_each_actual_sample_support() {
        for (width, height, rect, first, rows) in [
            (16000, 16000, [0, 0, 16000, 16000], 15993, 7),
            (3, 5, [-11, -17, 16000, 16000], 1, 3),
            (1901, 83, [-7, -9, 11, 19], 0, 67),
        ] {
            for method in [Resampling::Nearest, Resampling::Bilinear] {
                let mut grid =
                    Grid::new(&output(width, height, rect), method, first, rows).unwrap();
                let mut covered = vec![false; width as usize * rows as usize];
                while let Some(window) = grid.next().unwrap() {
                    let [l, t, r, b] = window.source;
                    assert!(r - l <= EDGE && b - t <= EDGE);
                    for y in window.output[1]..window.output[3] {
                        for x in window.output[0]..window.output[2] {
                            let at = ((y - first) * width + x) as usize;
                            assert!(!covered[at]);
                            covered[at] = true;
                            sample(grid.position(x, y).unwrap(), method, |sx, sy| {
                                assert!(sx >= l && sx < r && sy >= t && sy < b);
                                Ok([12347, 33559, 60001, 65535])
                            })
                            .unwrap();
                        }
                    }
                }
                assert!(covered.iter().all(|v| *v));
            }
        }
    }
    #[test]
    fn native_sampling_matches_actual_legacy_export_with_padding_and_low_bits() {
        for depth in [8, 16] {
            let mut words = Vec::new();
            for y in 0..7u16 {
                for x in 0..9u16 {
                    let p = [
                        12347 + x,
                        33559 + y,
                        60001 - x,
                        if (x + y) % 3 == 0 { 0 } else { 49153 },
                    ];
                    words.extend(if depth == 8 { p.map(|v| v / 257) } else { p });
                }
            }
            let pixels = if depth == 8 {
                Pixels::Eight(words.iter().map(|v| *v as u8).collect())
            } else {
                Pixels::Sixteen(words)
            };
            let image = Image {
                width: 9,
                height: 7,
                pixels,
            };
            for method in [Resampling::Nearest, Resampling::Bilinear] {
                for x in -13..53 {
                    for y in -9..43 {
                        let position = [f64::from(x) / 5., f64::from(y) / 5.];
                        assert_eq!(
                            sample(position, method, |x, y| Ok(image.sample(x, y))).unwrap(),
                            image.resampled(position[0], position[1], method)
                        );
                    }
                }
            }
        }
        assert_eq!(
            sample([0., 0.], Resampling::Bilinear, |_, _| Ok([
                12347, 33559, 60001, 0
            ]))
            .unwrap(),
            [12347, 33559, 60001, 0]
        );
        assert_eq!(
            sample([0.5, 0.5], Resampling::Bilinear, |_, _| Ok([
                12347, 33559, 60001, 0
            ]))
            .unwrap(),
            [0; 4]
        );
        assert!(sample([0.5, 0.5], Resampling::Bilinear, |_, _| Err(
            "missing prepared support".into()
        ))
        .is_err());
    }
    #[test]
    fn invalid_requests_refuse_before_subdivision() {
        let mut out = output(16000, 16000, [0, 0, 16000, 16000]);
        for (first, rows) in [(0, 0), (15999, 2), (0, 9), (u32::MAX, 1)] {
            assert!(Grid::new(&out, Resampling::Nearest, first, rows).is_err());
        }
        out.document_rect[0] = i32::MIN;
        assert!(Grid::new(&out, Resampling::Bilinear, 0, 1).is_err());
    }
}
