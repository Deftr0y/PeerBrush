//! Borrowed Pen presentation geometry. Commands still own all path editing.
use crate::{
    engine::{
        scene::{Content36, Source36},
        view::{LayerView, SourceView},
    },
    path::Anchor,
    source::Content,
};
#[derive(Clone, Copy)]
pub(in crate::ui) struct PathView<'a> {
    pub nodes: &'a [Anchor],
    pub closed: bool,
    matrix: [f32; 6],
    origin: [i32; 2],
}
impl<'a> PathView<'a> {
    pub fn layer(layer: LayerView<'a>) -> Option<Self> {
        let source = layer.source()?;
        let matrix = source.matrix();
        let origin = layer.origin();
        let (nodes, closed) = match source {
            SourceView::Resident(source) => match &source.content {
                Content::Bezier { nodes, closed, .. } => (nodes.as_slice(), *closed),
                _ => return None,
            },
            SourceView::Native(source) => return Self::native(source, origin),
        };
        Some(Self {
            nodes,
            closed,
            matrix,
            origin,
        })
    }
    pub fn native(source: &'a Source36, origin: [i32; 2]) -> Option<Self> {
        let Content36::Bezier { nodes, closed, .. } = source.definition() else {
            return None;
        };
        Some(Self {
            nodes,
            closed: *closed,
            matrix: source.matrix,
            origin,
        })
    }
    pub fn point(self, p: [f32; 2]) -> [f32; 2] {
        let m = self.matrix;
        [
            m[0] * p[0] + m[2] * p[1] + m[4] + self.origin[0] as f32,
            m[1] * p[0] + m[3] * p[1] + m[5] + self.origin[1] as f32,
        ]
    }
    pub fn nearest(self, point: [f32; 2], tolerance: f32) -> Option<(usize, f32)> {
        let count = self.nodes.len().saturating_sub(usize::from(!self.closed));
        let mut best = (tolerance * tolerance, None);
        for i in 0..count {
            let a = &self.nodes[i];
            let b = &self.nodes[(i + 1) % self.nodes.len()];
            let p = [
                a.point,
                a.out_handle.unwrap_or(a.point),
                b.in_handle.unwrap_or(b.point),
                b.point,
            ]
            .map(|p| self.point(p));
            let distance = |t: f32| {
                let u = 1. - t;
                let v = std::array::from_fn::<_, 2, _>(|axis| {
                    u * u * u * p[0][axis]
                        + 3. * u * u * t * p[1][axis]
                        + 3. * u * t * t * p[2][axis]
                        + t * t * t * p[3][axis]
                });
                (v[0] - point[0]).powi(2) + (v[1] - point[1]).powi(2)
            };
            let sample = (0..=64)
                .min_by(|a, b| distance(*a as f32 / 64.).total_cmp(&distance(*b as f32 / 64.)))
                .unwrap();
            let mut lo = (sample as f32 / 64. - 1. / 64.).max(0.001);
            let mut hi = (sample as f32 / 64. + 1. / 64.).min(0.999);
            for _ in 0..18 {
                let a = lo + (hi - lo) / 3.;
                let b = hi - (hi - lo) / 3.;
                if distance(a) < distance(b) {
                    hi = b;
                } else {
                    lo = a;
                }
            }
            let t = (lo + hi) * 0.5;
            let d = distance(t);
            if d < best.0 {
                best = (d, Some((i, t)));
            }
        }
        best.1
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn borrowed_geometry_matches_current_engine_hit_tests_bit_for_bit() {
        let mut layer = crate::engine::Layer::new("Pen geometry fixture", "raster", 1, 1);
        for matrix in [
            [1., 0., 0., 1., 0., 0.],
            [-0.75, 0.2, 0.125, 1.5, 0.35, -0.55],
            [0., 1., -1., 0., 2., 3.],
        ] {
            for closed in [false, true] {
                layer.x = -11;
                layer.y = 19;
                layer.source = Some(std::sync::Arc::new(crate::source::Source {
                    width: 96,
                    height: 80,
                    matrix,
                    content: Content::Bezier {
                        nodes: vec![
                            Anchor {
                                point: [1.25, 3.5],
                                in_handle: Some([-2.5, 0.125]),
                                out_handle: Some([17.25, 29.75]),
                                smooth: true,
                            },
                            Anchor {
                                point: [41.125, 37.75],
                                in_handle: Some([25.5, 61.75]),
                                out_handle: None,
                                smooth: false,
                            },
                            Anchor {
                                point: [74.25, 12.5],
                                in_handle: None,
                                out_handle: Some([95.25, -2.5]),
                                smooth: false,
                            },
                        ],
                        closed,
                        fill: [0; 4],
                        stroke: [1, 257, 65534, 65535],
                        stroke_width: 1.25,
                    },
                }));
                let view = PathView::layer((&layer).into()).unwrap();
                for point in [[0., 0.], [12.25, 18.5], [-8.5, 43.75], [37.125, 61.5]] {
                    assert_eq!(
                        view.point(point).map(f32::to_bits),
                        crate::path::document_point(&layer, point).map(f32::to_bits)
                    );
                    let actual = view.nearest(point, 500.);
                    let expected = crate::path::nearest_segment(&layer, point, 500.);
                    assert_eq!(
                        actual.map(|(i, t)| (i, t.to_bits())),
                        expected.map(|(i, t)| (i, t.to_bits()))
                    );
                }
            }
        }
    }
}
