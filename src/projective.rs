//! One document-space projective map, shared by geometry, native sampling and UI.
use serde_json::Value;

const ERROR: &str = "Invalid, folded or unstable projective transform";
const LIMIT: f64 = 100000.;

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Map([f64; 9]);

impl Map {
    pub(crate) const IDENTITY: Self = Self([1., 0., 0., 0., 1., 0., 0., 0., 1.]);

    pub(crate) fn from_matrix(matrix: [f64; 9]) -> Result<Self, String> {
        if matrix.iter().any(|v| !v.is_finite()) {
            return Err(ERROR.into());
        }
        let scale = matrix.iter().map(|v| v.abs()).fold(0., f64::max);
        if scale == 0. {
            return Err(ERROR.into());
        }
        let map = Self(matrix.map(|v| v / scale));
        map.adjugate()?;
        Ok(map)
    }

    pub(crate) fn matrix(self) -> [f64; 9] {
        self.0
    }

    pub(crate) fn affine_matrix([a, b, c, d, x, y]: [f64; 6]) -> Self {
        Self([a, c, x, b, d, y, 0., 0., 1.])
    }

    /// self * prior: the prior map is applied first. Never round perspective coefficients.
    pub(crate) fn compose(self, prior: Self) -> Result<Self, String> {
        let mut out = [0.; 9];
        for row in 0..3 {
            for column in 0..3 {
                out[row * 3 + column] = (0..3)
                    .map(|k| self.0[row * 3 + k] * prior.0[k * 3 + column])
                    .sum();
            }
        }
        Self::from_matrix(out)
    }

    fn adjugate(self) -> Result<([f64; 9], f64), String> {
        let [a, b, c, d, e, f, g, h, i] = self.0;
        let adj = [
            e * i - f * h,
            c * h - b * i,
            b * f - c * e,
            f * g - d * i,
            a * i - c * g,
            c * d - a * f,
            d * h - e * g,
            b * g - a * h,
            a * e - b * d,
        ];
        let terms = [a * adj[0], b * adj[3], c * adj[6]];
        let det: f64 = terms.iter().sum();
        let magnitude: f64 = terms.iter().map(|v| v.abs()).sum();
        // Relative cancellation, rather than a global matrix norm, admits large translations.
        if !det.is_finite()
            || det == 0.
            || det.abs() <= magnitude * 1e-12
            || adj.iter().any(|v| !v.is_finite())
        {
            return Err(ERROR.into());
        }
        Ok((adj, det))
    }

    pub(crate) fn inverse(self) -> Result<Self, String> {
        // The adjugate already represents the inverse up to homogeneous scale.
        Self::from_matrix(self.adjugate()?.0)
    }

    fn denominator(self, [x, y]: [f64; 2]) -> f64 {
        self.0[6] * x + self.0[7] * y + self.0[8]
    }

    pub(crate) fn point(self, [x, y]: [f64; 2]) -> Result<[f64; 2], String> {
        let m = self.0;
        let denominator = self.denominator([x, y]);
        let magnitude = (m[6] * x).abs() + (m[7] * y).abs() + m[8].abs();
        if !denominator.is_finite() || denominator.abs() <= magnitude * 1e-12 {
            return Err(ERROR.into());
        }
        let point = [
            (m[0] * x + m[1] * y + m[2]) / denominator,
            (m[3] * x + m[4] * y + m[5]) / denominator,
        ];
        if point.iter().any(|v| !v.is_finite()) {
            return Err(ERROR.into());
        }
        Ok(point)
    }

    /// A linear denominator takes its extrema at these corners. The complete
    /// original frame must pass, including transparent or currently clipped samples.
    pub(crate) fn bounds(self, rect: [f64; 4]) -> Result<[f64; 4], String> {
        let corners = corners(rect)?;
        let denominators = corners.map(|p| self.denominator(p));
        let largest = denominators.iter().map(|v| v.abs()).fold(0., f64::max);
        if largest == 0.
            || denominators.iter().any(|v| {
                !v.is_finite() || v.signum() != denominators[0].signum() || v.abs() < largest * 1e-6
            })
        {
            return Err(
                "Projective horizon intersects or approaches the complete source frame".into(),
            );
        }
        let mut bounds = [
            f64::INFINITY,
            f64::INFINITY,
            f64::NEG_INFINITY,
            f64::NEG_INFINITY,
        ];
        for p in corners {
            let point = self.point(p)?;
            if point.iter().any(|v| v.abs() > LIMIT) {
                return Err("Projective result is outside the coordinate limits".into());
            }
            for axis in 0..2 {
                // Enclose the actual f32 geometry used by selected polygon/coverage consumers too.
                bounds[axis] = bounds[axis]
                    .min(point[axis])
                    .min(f64::from(point[axis] as f32));
                bounds[axis + 2] = bounds[axis + 2]
                    .max(point[axis])
                    .max(f64::from(point[axis] as f32));
            }
        }
        Ok(bounds)
    }

    /// Only the sampling domain may use transparent outside coordinates. Geometry
    /// and persisted-source validation always propagate errors instead.
    pub(crate) fn sample(self, point: [f64; 2]) -> [f32; 2] {
        self.point(point)
            .ok()
            .filter(|p| p.iter().all(|v| v.abs() <= 1e6))
            .map(|p| p.map(|v| v as f32))
            .unwrap_or([-2.; 2])
    }

    pub(crate) fn as_affine(self) -> Option<[f64; 6]> {
        let m = self.0;
        (m[6] == 0. && m[7] == 0. && m[8] != 0.).then(|| {
            [
                m[0] / m[8],
                m[3] / m[8],
                m[1] / m[8],
                m[4] / m[8],
                m[2] / m[8],
                m[5] / m[8],
            ]
        })
    }
}

pub(crate) fn corners([l, t, r, b]: [f64; 4]) -> Result<[[f64; 2]; 4], String> {
    if [l, t, r, b]
        .iter()
        .any(|v| !v.is_finite() || v.abs() > LIMIT)
        || r - l < 1e-6
        || b - t < 1e-6
    {
        return Err(ERROR.into());
    }
    Ok([[l, t], [r, t], [r, b], [l, b]])
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct Specification {
    pub(crate) rect: [f64; 4],
    pub(crate) quad: [[f64; 2]; 4],
    pub(crate) map: Map,
}

impl Specification {
    pub(crate) fn center(self) -> [f64; 2] {
        [
            (self.rect[0] + self.rect[2]) * 0.5,
            (self.rect[1] + self.rect[3]) * 0.5,
        ]
    }
}

/// P maps document rect corners TL/TR/BR/BL to the supplied same-winding quad.
/// Reflection belongs to the existing affine mirror flags, never a folded quad.
pub(crate) fn parse(command: &Value) -> Result<Option<Specification>, String> {
    let Some(value) = command.get("projective") else {
        return Ok(None);
    };
    if command["op"] != "transform" {
        return Err("Projective placement requires transform".into());
    }
    let fields = value
        .as_object()
        .filter(|v| v.len() == 2 && v.contains_key("rect") && v.contains_key("quad"))
        .ok_or("Projective placement requires only rect and quad")?;
    let rect: [f64; 4] = serde_json::from_value(fields["rect"].clone()).map_err(|_| ERROR)?;
    let quad: [[f64; 2]; 4] = serde_json::from_value(fields["quad"].clone()).map_err(|_| ERROR)?;
    let source = corners(rect)?;
    if quad
        .iter()
        .flatten()
        .any(|v| !v.is_finite() || v.abs() > LIMIT)
    {
        return Err(ERROR.into());
    }
    for index in 0..4 {
        let p = quad[index];
        let q = quad[(index + 1) % 4];
        let r = quad[(index + 2) % 4];
        let a = [q[0] - p[0], q[1] - p[1]];
        let b = [r[0] - q[0], r[1] - q[1]];
        let lengths = a[0].hypot(a[1]) * b[0].hypot(b[1]);
        if lengths < 1e-12 || a[0] * b[1] - a[1] * b[0] <= lengths * 1e-10 {
            return Err(ERROR.into());
        }
    }
    if source == quad {
        return Ok(None);
    }
    let [p0, p1, p2, p3] = quad;
    let (x1, x2, x3) = (p1[0] - p2[0], p3[0] - p2[0], p0[0] - p1[0] + p2[0] - p3[0]);
    let (y1, y2, y3) = (p1[1] - p2[1], p3[1] - p2[1], p0[1] - p1[1] + p2[1] - p3[1]);
    let (g, h) = if x3 == 0. && y3 == 0. {
        (0., 0.)
    } else {
        let denominator = x1 * y2 - x2 * y1;
        if denominator.abs() <= (x1 * y2).abs().max((x2 * y1).abs()) * 1e-12 {
            return Err(ERROR.into());
        }
        (
            (x3 * y2 - x2 * y3) / denominator,
            (x1 * y3 - x3 * y1) / denominator,
        )
    };
    let square = Map::from_matrix([
        p1[0] - p0[0] + g * p1[0],
        p3[0] - p0[0] + h * p3[0],
        p0[0],
        p1[1] - p0[1] + g * p1[1],
        p3[1] - p0[1] + h * p3[1],
        p0[1],
        g,
        h,
        1.,
    ])?;
    let (w, h) = (rect[2] - rect[0], rect[3] - rect[1]);
    let map = square.compose(Map::affine_matrix([
        1. / w,
        0.,
        0.,
        1. / h,
        -rect[0] / w,
        -rect[1] / h,
    ]))?;
    map.bounds(rect)?;
    for (p, q) in source.into_iter().zip(quad) {
        let actual = map.point(p)?;
        if (0..2).any(|axis| (actual[axis] - q[axis]).abs() > 1e-7 * (1. + q[axis].abs())) {
            return Err(ERROR.into());
        }
    }
    Ok(Some(Specification { rect, quad, map }))
}

/// Existing affine interpretation, after P. Projective commands default the
/// pivot to the explicit rect center so multiple roots share one document map.
pub(crate) fn affine(command: &Value, default_pivot: [f64; 2]) -> Result<Map, String> {
    let angle = command.get("angle").map_or(Ok(0.), |v| {
        v.as_f64().ok_or("Transform angle must be a number")
    })?;
    let pivot: [f64; 2] = command.get("pivot").map_or(Ok(default_pivot), |v| {
        serde_json::from_value(v.clone()).map_err(|_| "Transform pivot requires two numbers")
    })?;
    if !angle.is_finite()
        || angle.abs() > LIMIT
        || pivot.iter().any(|v| !v.is_finite() || v.abs() > LIMIT)
    {
        return Err("Invalid projective affine angle or pivot".into());
    }
    let (sx, sy) = crate::transform::scales(command)?;
    let [dx, dy] = crate::transform::translation(command)?;
    let (sin, cos) = angle.to_radians().sin_cos();
    let [a, b, c, d] =
        crate::transform::skewed([cos * sx, sin * sx, -sin * sy, cos * sy], command)?;
    Map::from_matrix([
        a,
        c,
        pivot[0] - a * pivot[0] - c * pivot[1] + dx,
        b,
        d,
        pivot[1] - b * pivot[0] - d * pivot[1] + dy,
        0.,
        0.,
        1.,
    ])
}

pub(crate) fn combined(command: &Value) -> Result<Option<Map>, String> {
    parse(command)?
        .map(|p| affine(command, p.center())?.compose(p.map))
        .transpose()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn command(rect: [f64; 4], quad: [[f64; 2]; 4]) -> Value {
        json!({"op":"transform","projective":{"rect":rect,"quad":quad}})
    }

    #[test]
    fn fitted_perspective_and_composed_affine_match_independent_analytic_points() {
        let rect = [0., 0., 32., 24.];
        let project = |[x, y]: [f64; 2]| [x / (1. + x / 64.), y / (1. + x / 64.)];
        let mut c = command(rect, corners(rect).unwrap().map(project));
        c["scale_x"] = json!(1.5);
        c["scale_y"] = json!(0.75);
        c["pivot"] = json!([0., 0.]);
        c["dx"] = json!(3.);
        let map = combined(&c).unwrap().unwrap();
        for point in [[0., 0.], [32., 24.], [7.5, 9.25], [19.75, 3.5]] {
            let p = project(point);
            let expected = [p[0] * 1.5 + 3., p[1] * 0.75];
            let actual = map.point(point).unwrap();
            let restored = map.inverse().unwrap().point(actual).unwrap();
            for axis in 0..2 {
                assert!((actual[axis] - expected[axis]).abs() < 1e-10);
                assert!((restored[axis] - point[axis]).abs() < 1e-10);
            }
        }
    }

    #[test]
    fn tiny_perspective_large_offset_thin_frames_and_zero_world_h33_remain_real_maps() {
        let small = Map::from_matrix([1., 0., 0., 0., 1., 0., 1e-9, 0., 1.]).unwrap();
        assert!(small.as_affine().is_none());
        assert!(8192. - small.point([8192., 0.]).unwrap()[0] > 0.05);
        small.bounds([0., 0., 8192., 1.]).unwrap();
        let rect = [50000., 40000., 58192., 40001.];
        let translated = corners(rect).unwrap().map(|[x, y]| [x + 2., y + 3.]);
        let p = parse(&command(rect, translated)).unwrap().unwrap();
        p.map.bounds(rect).unwrap();
        for (point, expected) in corners(rect).unwrap().into_iter().zip(translated) {
            let actual = p.map.point(point).unwrap();
            for axis in 0..2 {
                assert!((actual[axis] - expected[axis]).abs() < 1e-7);
            }
        }
        let zero = Map::from_matrix([1., 0., -1., 0., 1., 0., 0.01, 0., 0.]).unwrap();
        let rect = [10., 1., 20., 5.];
        zero.bounds(rect).unwrap();
        let p = parse(&command(
            rect,
            corners(rect).unwrap().map(|v| zero.point(v).unwrap()),
        ))
        .unwrap()
        .unwrap();
        assert!(p.map.as_affine().is_none());
        assert!(p.map.bounds([0., 0., 20., 5.]).is_err());
    }

    #[test]
    fn malformed_folded_near_horizon_and_unknown_fields_reject_without_identity_approximation() {
        let rect = [0., 0., 32., 24.];
        let identity = command(rect, corners(rect).unwrap());
        assert!(parse(&identity).unwrap().is_none());
        assert!(parse(&json!({"op":"transform"})).unwrap().is_none());
        for value in [
            json!({"rect":rect,"quad":[[0,0],[32,24],[32,0],[0,24]]}),
            json!({"rect":rect,"quad":[[0,0],[32,0],[32,0],[0,24]]}),
            json!({"rect":rect,"quad":[[0,0],[32,0],[32,24],[0,24]],"extra":true}),
            json!({"rect":[0,0,0,24],"quad":[[0,0],[32,0],[32,24],[0,24]]}),
            Value::Null,
        ] {
            assert!(parse(&json!({"op":"transform","projective":value})).is_err());
        }
        let crossing = Map::from_matrix([1., 0., 0., 0., 1., 0., -1. / 16., 0., 1.]).unwrap();
        assert!(crossing.bounds(rect).is_err());
        assert_eq!(crossing.sample([16., 1.]), [-2.; 2]);
        assert!(Map::from_matrix([1., 0., 0., 0., 0., 0., 0., 0., 1.]).is_err());
        assert!(Map::from_matrix([f64::NAN; 9]).is_err());
    }
}
