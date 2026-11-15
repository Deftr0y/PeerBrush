//! Native analytic shape coverage at the original source-grid phase.
use crate::raster::{project16, Pixel16};
#[derive(Clone, Copy)]
pub(super) struct Shape<'a> {
    pub kind: &'a str,
    pub bounds: [f32; 4],
    pub points: &'a [[f32; 2]],
    pub closed: bool,
    pub fill: Pixel16,
    pub stroke: Pixel16,
    pub stroke_width: f32,
}
impl Shape<'_> {
    pub(super) fn bounds(&self, frame: [u32; 2]) -> [u32; 4] {
        let b = if self.kind == "path" {
            [
                self.points
                    .iter()
                    .map(|p| p[0])
                    .fold(f32::INFINITY, f32::min),
                self.points
                    .iter()
                    .map(|p| p[1])
                    .fold(f32::INFINITY, f32::min),
                self.points
                    .iter()
                    .map(|p| p[0])
                    .fold(f32::NEG_INFINITY, f32::max),
                self.points
                    .iter()
                    .map(|p| p[1])
                    .fold(f32::NEG_INFINITY, f32::max),
            ]
        } else {
            self.bounds
        };
        let half = self.stroke_width * 0.5;
        [
            (b[0] - half - 1.).floor().max(0.) as u32,
            (b[1] - half - 1.).floor().max(0.) as u32,
            (b[2] + half + 1.).ceil().min(frame[0] as f32).max(0.) as u32,
            (b[3] + half + 1.).ceil().min(frame[1] as f32).max(0.) as u32,
        ]
    }
    pub(super) fn pixel(&self, b: [u32; 4], x: i32, y: i32, depth: u16) -> Pixel16 {
        let (x, y) = (x as u32, y as u32);
        if x < b[0] || y < b[1] || x >= b[2] || y >= b[3] {
            return [0; 4];
        }
        let (shape, bounds, points, closed, fill, stroke, stroke_width) = (
            self.kind,
            &self.bounds,
            self.points,
            &self.closed,
            &self.fill,
            &self.stroke,
            &self.stroke_width,
        );
        let half = *stroke_width * 0.5;
        let mut fc = 0.;
        let mut sc = 0.;
        for oy in [0.125, 0.375, 0.625, 0.875] {
            for ox in [0.125, 0.375, 0.625, 0.875] {
                let p = [x as f32 + ox, y as f32 + oy];
                let (inside, distance) = if shape == "ellipse" {
                    let cx = (bounds[0] + bounds[2]) * 0.5;
                    let cy = (bounds[1] + bounds[3]) * 0.5;
                    let rx = (bounds[2] - bounds[0]) * 0.5;
                    let ry = (bounds[3] - bounds[1]) * 0.5;
                    let u = (p[0] - cx) / rx;
                    let v = (p[1] - cy) / ry;
                    let len = (u * u + v * v).sqrt();
                    // Distance to ellipse from implicit gradient; stable at its center.
                    let grad = ((u / rx).powi(2) + (v / ry).powi(2)).sqrt();
                    (
                        len <= 1.,
                        if grad > 0.000001 {
                            (len - 1.).abs() * len / grad
                        } else {
                            rx.min(ry)
                        },
                    )
                } else {
                    let rect = [
                        [bounds[0], bounds[1]],
                        [bounds[2], bounds[1]],
                        [bounds[2], bounds[3]],
                        [bounds[0], bounds[3]],
                    ];
                    let vertices = if shape == "path" { points } else { &rect };
                    let closed = shape != "path" || *closed;
                    let inside = closed && crate::selection::contains(vertices, p[0], p[1]);
                    let count = vertices.len() - usize::from(!closed);
                    let distance = (0..count)
                        .map(|i| {
                            segment_distance(p, vertices[i], vertices[(i + 1) % vertices.len()])
                        })
                        .fold(f32::INFINITY, f32::min);
                    (inside, distance)
                };
                if inside {
                    fc += 1. / 16.;
                }
                if *stroke_width > 0. && distance <= half {
                    sc += 1. / 16.;
                }
            }
        }
        let mut f = *fill;
        f[3] = (f32::from(f[3]) * fc).round() as u16;
        let mut s = *stroke;
        s[3] = (f32::from(s[3]) * sc).round() as u16;
        let p = crate::raster::blend16(f, s, 1., "normal");
        if p[3] == 0 {
            [0; 4]
        } else if depth == 8 {
            p.map(|v| u16::from(project16(v)) * 257)
        } else {
            p
        }
    }
}
fn segment_distance(p: [f32; 2], a: [f32; 2], b: [f32; 2]) -> f32 {
    let dx = b[0] - a[0];
    let dy = b[1] - a[1];
    let len = dx * dx + dy * dy;
    let t = if len > 0. {
        ((p[0] - a[0]) * dx + (p[1] - a[1]) * dy) / len
    } else {
        0.
    }
    .clamp(0., 1.);
    ((p[0] - a[0] - t * dx).powi(2) + (p[1] - a[1] - t * dy).powi(2)).sqrt()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source::{Content, Source};
    #[test]
    fn analytic_native_shape_samples_match_existing_source_renderer_exactly() {
        let frame = [61, 67];
        let points = vec![[3.25, 4.5], [51.75, 9.25], [43.5, 59.75], [8.125, 47.25]];
        for kind in ["rectangle", "ellipse", "path"] {
            for closed in [false, true] {
                for depth in [8, 16] {
                    let shape = Shape {
                        kind,
                        bounds: [4.25, 5.75, 53.375, 58.625],
                        points: &points,
                        closed,
                        fill: [12347, 33559, 51237, 45679],
                        stroke: [54321, 12345, 45679, 27139],
                        stroke_width: 3.5,
                    };
                    let source = Source {
                        width: frame[0],
                        height: frame[1],
                        matrix: [1., 0., 0., 1., 0., 0.],
                        content: Content::Shape {
                            shape: kind.into(),
                            bounds: shape.bounds,
                            points: points.clone(),
                            closed,
                            fill: shape.fill,
                            stroke: shape.stroke,
                            stroke_width: shape.stroke_width,
                        },
                    };
                    let reference = source.render(frame[0], frame[1], depth).unwrap();
                    let bounds = shape.bounds(frame);
                    for y in -1..=frame[1] as i32 {
                        for x in -1..=frame[0] as i32 {
                            assert_eq!(
                                shape.pixel(bounds, x, y, depth),
                                reference.get16(x, y),
                                "{kind} closed={closed} native{depth}({x},{y})"
                            );
                        }
                    }
                }
            }
        }
    }
    #[test]
    fn large_frame_metadata_keeps_quarter_pixel_source_phase_without_large_allocation() {
        let points = [];
        let small = Shape {
            kind: "ellipse",
            bounds: [4.25, 5.75, 53.375, 58.625],
            points: &points,
            closed: true,
            fill: [12347, 33559, 51237, 45679],
            stroke: [54321, 12345, 45679, 27139],
            stroke_width: 3.5,
        };
        let mut shifted = small;
        shifted.bounds = small.bounds.map(|v| v + 15000.);
        let small_bounds = small.bounds([61, 67]);
        let shifted_bounds = shifted.bounds([16000, 16000]);
        for depth in [8, 16] {
            for y in 0..64 {
                for x in 0..61 {
                    assert_eq!(
                        shifted.pixel(shifted_bounds, x + 15000, y + 15000, depth),
                        small.pixel(small_bounds, x, y, depth)
                    );
                }
            }
        }
        // This is a small geometric phase check, not a dense16K/editor run.
    }
}
