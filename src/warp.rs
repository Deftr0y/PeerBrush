//! A coarse editable 3x3 grid. Eight exact affine triangles, not Bezier patches.
use crate::projective::Map;
use serde::{Deserialize, Serialize};
use serde_json::Value;
mod command;
pub(crate) use command::{domain, CommandMap};

pub const MAX_STAGES: usize = 16;
pub(crate) const MAX_VERTICES: usize = 8192;
const ERROR: &str = "Invalid, folded or unstable Warp grid";
const EPSILON: f64 = 1e-10;
const TRIANGLES: [[usize; 3]; 8] = [
    [0, 1, 4],
    [0, 4, 3],
    [1, 2, 5],
    [1, 5, 4],
    [3, 4, 7],
    [3, 7, 6],
    [4, 5, 8],
    [4, 8, 7],
];

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Grid {
    pub rect: [f64; 4],
    pub points: [[f64; 2]; 9],
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Stage {
    pub before: [f64; 9],
    pub grid: Grid,
}
pub(crate) fn stages<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Vec<Stage>, D::Error> {
    struct Stages;
    impl<'de> serde::de::Visitor<'de> for Stages {
        type Value = Vec<Stage>;
        fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("at most 16 retained Warp stages")
        }
        fn visit_seq<A: serde::de::SeqAccess<'de>>(
            self,
            mut sequence: A,
        ) -> Result<Self::Value, A::Error> {
            let mut values = Vec::new();
            while let Some(stage) = sequence.next_element::<Stage>()? {
                if values.len() == MAX_STAGES {
                    return Err(serde::de::Error::custom(
                        "Retained Warp stage limit exceeded",
                    ));
                }
                values.push(stage);
            }
            Ok(values)
        }
    }
    deserializer.deserialize_seq(Stages)
}

fn subtract(a: [f64; 2], b: [f64; 2]) -> [f64; 2] {
    [a[0] - b[0], a[1] - b[1]]
}
fn cross(a: [f64; 2], b: [f64; 2]) -> f64 {
    a[0] * b[1] - a[1] * b[0]
}
fn dot(a: [f64; 2], b: [f64; 2]) -> f64 {
    a[0] * b[0] + a[1] * b[1]
}
fn blend(p: [f64; 2], q: [f64; 2], t: f64) -> [f64; 2] {
    [p[0] + (q[0] - p[0]) * t, p[1] + (q[1] - p[1]) * t]
}
fn finite(p: [f64; 2]) -> bool {
    p.iter().all(|v| v.is_finite() && v.abs() <= 100000.)
}
fn extrema(points: &[[f64; 2]]) -> [f64; 4] {
    points.iter().fold(
        [
            f64::INFINITY,
            f64::INFINITY,
            f64::NEG_INFINITY,
            f64::NEG_INFINITY,
        ],
        |mut b, p| {
            for a in 0..2 {
                b[a] = b[a].min(p[a]).min(f64::from(p[a] as f32));
                b[a + 2] = b[a + 2].max(p[a]).max(f64::from(p[a] as f32));
            }
            b
        },
    )
}

#[derive(Clone, Copy)]
struct Triangle {
    source: [[f64; 2]; 3],
    target: [[f64; 2]; 3],
}
impl Triangle {
    fn weights(points: [[f64; 2]; 3], p: [f64; 2]) -> [f64; 2] {
        let u = subtract(points[1], points[0]);
        let v = subtract(points[2], points[0]);
        let p = subtract(p, points[0]);
        let d = cross(u, v);
        [cross(p, v) / d, cross(u, p) / d]
    }
    fn at(points: [[f64; 2]; 3], [u, v]: [f64; 2]) -> [f64; 2] {
        [
            points[0][0] + u * (points[1][0] - points[0][0]) + v * (points[2][0] - points[0][0]),
            points[0][1] + u * (points[1][1] - points[0][1]) + v * (points[2][1] - points[0][1]),
        ]
    }
    fn forward(self, p: [f64; 2]) -> [f64; 2] {
        Self::at(self.target, Self::weights(self.source, p))
    }
    fn inverse(self, p: [f64; 2]) -> Option<[f64; 2]> {
        let [u, v] = Self::weights(self.target, p);
        if !u.is_finite() || !v.is_finite() || u < -EPSILON || v < -EPSILON || u + v > 1. + EPSILON
        {
            return None;
        }
        let u = u.max(0.).min(1.);
        let v = v.max(0.).min(1. - u);
        Some(Self::at(self.source, [u, v]))
    }
}

#[derive(Clone)]
pub(crate) struct Mapping {
    grid: Grid,
    triangles: [Triangle; 8],
}
impl Grid {
    pub fn identity(rect: [f64; 4]) -> Result<Self, String> {
        crate::projective::corners(rect)?;
        let points = std::array::from_fn(|i| {
            [
                rect[0] + (rect[2] - rect[0]) * (i % 3) as f64 / 2.,
                rect[1] + (rect[3] - rect[1]) * (i / 3) as f64 / 2.,
            ]
        });
        Ok(Self { rect, points })
    }
    pub fn preset(rect: [f64; 4], preset: &str, amount: f64) -> Result<Self, String> {
        if !amount.is_finite() || amount.abs() > 0.25 {
            return Err("Warp preset amount must be between -0.25 and 0.25".into());
        }
        let mut grid = Self::identity(rect)?;
        let (w, h) = (rect[2] - rect[0], rect[3] - rect[1]);
        for (i, p) in grid.points.iter_mut().enumerate() {
            let (u, v) = ((i % 3) as f64 - 1., (i / 3) as f64 - 1.);
            match preset {
                "arc" => p[1] += amount * h * (1. - u * u),
                "bulge" => {
                    p[0] += amount * w * u * (1. - v * v);
                    p[1] += amount * h * v * (1. - u * u);
                }
                "flag" => p[1] += amount * h * u * (1. - v * v),
                _ => return Err("Warp preset must be arc, bulge or flag".into()),
            }
        }
        if !grid.is_identity() {
            grid.prepare()?;
        }
        Ok(grid)
    }
    pub fn is_identity(&self) -> bool {
        Self::identity(self.rect).is_ok_and(|g| g.points == self.points)
    }
    pub(crate) fn prepare(&self) -> Result<Mapping, String> {
        let source = Self::identity(self.rect)?.points;
        if self.points.iter().any(|&p| !finite(p)) {
            return Err(ERROR.into());
        }
        let triangles = TRIANGLES.map(|ids| Triangle {
            source: ids.map(|i| source[i]),
            target: ids.map(|i| self.points[i]),
        });
        for triangle in triangles {
            let u = subtract(triangle.target[1], triangle.target[0]);
            let v = subtract(triangle.target[2], triangle.target[0]);
            let third = subtract(v, u);
            let largest = dot(u, u).max(dot(v, v)).max(dot(third, third));
            if largest < 1e-12 || cross(u, v) <= largest * 1e-6 {
                return Err(ERROR.into());
            }
        }
        let edges = edges();
        for (i, &[a, b]) in edges.iter().enumerate() {
            for &[c, d] in &edges[i + 1..] {
                let (p, q, r, s) = (
                    self.points[a],
                    self.points[b],
                    self.points[c],
                    self.points[d],
                );
                let shared = [a, b].into_iter().find(|n| *n == c || *n == d);
                if let Some(n) = shared {
                    let first = subtract(self.points[if a == n { b } else { a }], self.points[n]);
                    let second = subtract(self.points[if c == n { d } else { c }], self.points[n]);
                    if cross(first, second).abs()
                        <= EPSILON * first[0].hypot(first[1]) * second[0].hypot(second[1])
                        && dot(first, second) > 0.
                    {
                        return Err(ERROR.into());
                    }
                } else if intersects(p, q, r, s) {
                    return Err(ERROR.into());
                }
            }
        }
        // No crossing/touching non-neighbor edges and positive triangle areas
        // leave only complete triangle containment to exclude.
        for (i, ids) in TRIANGLES.iter().enumerate() {
            for (j, other) in TRIANGLES.iter().enumerate() {
                if i == j {
                    continue;
                }
                for &node in other {
                    if !ids.contains(&node) {
                        let [u, v] = Triangle::weights(triangles[i].target, self.points[node]);
                        if u >= -EPSILON && v >= -EPSILON && u + v <= 1. + EPSILON {
                            return Err(ERROR.into());
                        }
                    }
                }
            }
        }
        Ok(Mapping {
            grid: self.clone(),
            triangles,
        })
    }
}
fn edges() -> Vec<[usize; 2]> {
    let mut edges = Vec::with_capacity(16);
    for t in TRIANGLES {
        for i in 0..3 {
            let (a, b) = (t[i], t[(i + 1) % 3]);
            let edge = [a.min(b), a.max(b)];
            if !edges.contains(&edge) {
                edges.push(edge);
            }
        }
    }
    edges
}
fn intersects(p: [f64; 2], q: [f64; 2], r: [f64; 2], s: [f64; 2]) -> bool {
    let a = subtract(q, p);
    let b = subtract(s, r);
    let d = cross(a, b);
    let delta = subtract(r, p);
    if d == 0. {
        if cross(a, delta) != 0. {
            return false;
        }
        let axis = usize::from(a[1].abs() > a[0].abs());
        return p[axis].min(q[axis]) <= r[axis].max(s[axis])
            && r[axis].min(s[axis]) <= p[axis].max(q[axis]);
    }
    let (u, v) = (cross(delta, b) / d, cross(delta, a) / d);
    (-EPSILON..=1. + EPSILON).contains(&u) && (-EPSILON..=1. + EPSILON).contains(&v)
}

impl Mapping {
    pub(crate) fn covers(&self, b: [f64; 4]) -> Result<(), String> {
        if b.iter().any(|n| !n.is_finite())
            || b[0] >= b[2]
            || b[1] >= b[3]
            || b[0] < self.grid.rect[0]
            || b[1] < self.grid.rect[1]
            || b[2] > self.grid.rect[2]
            || b[3] > self.grid.rect[3]
        {
            return Err("Warp grid must cover the complete retained source and interpolation-support domain".into());
        }
        Ok(())
    }
    pub(crate) fn point(&self, p: [f64; 2]) -> Result<[f64; 2], String> {
        if !finite(p)
            || p[0] < self.grid.rect[0]
            || p[1] < self.grid.rect[1]
            || p[0] > self.grid.rect[2]
            || p[1] > self.grid.rect[3]
        {
            return Err(ERROR.into());
        }
        let u = (p[0] - self.grid.rect[0]) * 2. / (self.grid.rect[2] - self.grid.rect[0]);
        let v = (p[1] - self.grid.rect[1]) * 2. / (self.grid.rect[3] - self.grid.rect[1]);
        let (x, y) = (u.floor().min(1.) as usize, v.floor().min(1.) as usize);
        let triangle = (y * 2 + x) * 2 + usize::from(v - y as f64 > u - x as f64);
        let out = self.triangles[triangle].forward(p);
        if !finite(out) {
            return Err(ERROR.into());
        }
        Ok(out)
    }
    pub(crate) fn inverse(&self, p: [f64; 2]) -> Option<[f64; 2]> {
        finite(p).then_some(())?;
        self.triangles.iter().find_map(|t| t.inverse(p))
    }
    pub(crate) fn polygon(&self, points: &[[f64; 2]]) -> Result<Vec<[f64; 2]>, String> {
        if !(3..=MAX_VERTICES).contains(&points.len()) {
            return Err("Warp polygon exceeds the 8192-vertex limit".into());
        }
        let seams = edges();
        // A source edge crosses each of the fixed 16 edges at most once.
        if points
            .len()
            .checked_mul(seams.len() + 1)
            .is_none_or(|n| n > 131072)
        {
            return Err("Warp polygon exceeds the bounded seam-work limit".into());
        }
        let source = Grid::identity(self.grid.rect)?.points;
        let mut out = Vec::with_capacity(points.len().min(MAX_VERTICES));
        for (i, &p) in points.iter().enumerate() {
            let q = points[(i + 1) % points.len()];
            self.point(p)?;
            self.point(q)?;
            let direction = subtract(q, p);
            let mut cuts = vec![0.];
            for &[a, b] in &seams {
                let edge = subtract(source[b], source[a]);
                let d = cross(direction, edge);
                if d == 0. {
                    continue;
                }
                let delta = subtract(source[a], p);
                let (t, u) = (cross(delta, edge) / d, cross(delta, direction) / d);
                if t > 0. && t < 1. && (-EPSILON..=1. + EPSILON).contains(&u) {
                    cuts.push(t);
                }
            }
            cuts.sort_by(f64::total_cmp);
            cuts.dedup();
            if out.len() + cuts.len() > MAX_VERTICES {
                return Err("Warp polygon exceeds the 8192-vertex limit".into());
            }
            for t in cuts {
                out.push(self.point(blend(p, q, t))?);
            }
        }
        Ok(out)
    }
    pub(crate) fn bounds(&self, b: [f64; 4]) -> Result<[f64; 4], String> {
        self.covers(b)?;
        let mut out = Vec::with_capacity(64);
        for triangle in self.triangles {
            let mut polygon = triangle.source.to_vec();
            for (axis, limit, greater) in [
                (0, b[0], true),
                (0, b[2], false),
                (1, b[1], true),
                (1, b[3], false),
            ] {
                let mut clipped = Vec::with_capacity(8);
                for i in 0..polygon.len() {
                    let (p, q) = (polygon[i], polygon[(i + 1) % polygon.len()]);
                    let inside = |p: [f64; 2]| {
                        if greater {
                            p[axis] >= limit
                        } else {
                            p[axis] <= limit
                        }
                    };
                    if inside(p) {
                        clipped.push(p);
                    }
                    if inside(p) != inside(q) {
                        clipped.push(blend(p, q, (limit - p[axis]) / (q[axis] - p[axis])));
                    }
                }
                polygon = clipped;
            }
            out.extend(polygon.into_iter().map(|p| triangle.forward(p)));
        }
        if out.is_empty() || out.iter().any(|&p| !finite(p)) {
            return Err(ERROR.into());
        }
        Ok(extrema(&out))
    }
}

pub(crate) fn parse(command: &Value) -> Result<Option<Grid>, String> {
    let Some(value) = command.get("warp") else {
        return Ok(None);
    };
    if command["op"] != "transform" {
        return Err("Warp requires a transform command".into());
    }
    let fields = value
        .as_object()
        .ok_or("Warp requires rect and points, or rect/preset/amount")?;
    let grid = if fields.len() == 2 && fields.contains_key("rect") && fields.contains_key("points")
    {
        serde_json::from_value::<Grid>(value.clone()).map_err(|_| ERROR.to_string())?
    } else if fields.len() == 3
        && fields.contains_key("rect")
        && fields.contains_key("preset")
        && fields.contains_key("amount")
    {
        let rect = serde_json::from_value(fields["rect"].clone()).map_err(|_| ERROR.to_string())?;
        Grid::preset(
            rect,
            fields["preset"].as_str().ok_or(ERROR)?,
            fields["amount"].as_f64().ok_or(ERROR)?,
        )?
    } else {
        return Err("Warp requires only rect/points or rect/preset/amount".into());
    };
    Grid::identity(grid.rect)?;
    if grid.points.iter().any(|&p| !finite(p)) {
        return Err(ERROR.into());
    }
    if grid.is_identity() {
        return Ok(None);
    }
    grid.prepare()?;
    Ok(Some(grid))
}

struct PreparedStage {
    before: Map,
    inverse: Map,
    grid: Mapping,
}
pub(crate) struct Placement {
    stages: Vec<PreparedStage>,
    after: Map,
    inverse: Map,
}
impl Placement {
    pub(crate) fn new(stages: &[Stage], after: Map) -> Result<Self, String> {
        if stages.len() > MAX_STAGES {
            return Err("Retained Warp stage limit reached; undo an earlier Warp or cancel".into());
        }
        let stages = stages
            .iter()
            .map(|s| {
                let before = Map::from_matrix(s.before)?;
                Ok(PreparedStage {
                    before,
                    inverse: before.inverse()?,
                    grid: s.grid.prepare()?,
                })
            })
            .collect::<Result<Vec<_>, String>>()?;
        Ok(Self {
            stages,
            after,
            inverse: after.inverse()?,
        })
    }
    pub(crate) fn bounds(&self, mut b: [f64; 4]) -> Result<[f64; 4], String> {
        for s in &self.stages {
            b = s.grid.bounds(s.before.bounds(b)?)?;
        }
        self.after.bounds(b)
    }
    pub(crate) fn source(&self, point: [f64; 2]) -> Option<[f64; 2]> {
        let mut p = self.inverse.point(point).ok()?;
        for stage in self.stages.iter().rev() {
            p = stage.inverse.point(stage.grid.inverse(p)?).ok()?;
        }
        Some(p)
    }
    pub(crate) fn sample(&self, point: [f64; 2]) -> [f32; 2] {
        self.source(point)
            .filter(|p| p.iter().all(|n| n.is_finite() && n.abs() < 1e6))
            .map(|[x, y]| [(x - 0.5) as f32, (y - 0.5) as f32])
            .unwrap_or([-2.; 2])
    }
}

pub(crate) fn work_budget(
    width: u32,
    height: u32,
    rasters: usize,
    stages: usize,
    vertices: usize,
) -> Result<(), String> {
    // Eight triangle probes plus each stage's preceding homography, then the final map.
    let cost = (stages as u64)
        .checked_mul(9)
        .and_then(|n| n.checked_add(vertices as u64))
        .and_then(|n| n.checked_add(1));
    let work = u64::from(width)
        .checked_mul(u64::from(height))
        .and_then(|n| n.checked_mul(rasters as u64))
        .and_then(|n| cost.and_then(|c| n.checked_mul(c)));
    if stages > MAX_STAGES || work.is_none_or(|n| n > 128_000_000) {
        return Err("Warp exceeds the 128-million mapping-work budget".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn arc_has_independent_piecewise_analytic_forward_inverse_and_seam_geometry() {
        let grid = Grid::preset([0., 0., 16., 12.], "arc", 0.25).unwrap();
        let map = grid.prepare().unwrap();
        let analytic = |[x, y]: [f64; 2]| [x, y + 3. * (1. - (x / 8. - 1.).abs())];
        for p in [[0., 0.], [16., 12.], [4., 2.], [8., 6.], [13.25, 9.5]] {
            let q = map.point(p).unwrap();
            let expected = analytic(p);
            for a in 0..2 {
                assert!((q[a] - expected[a]).abs() < 1e-10);
                assert!((map.inverse(q).unwrap()[a] - p[a]).abs() < 1e-10);
            }
        }
        let rect = [[0., 0.], [16., 0.], [16., 12.], [0., 12.]];
        let polygon = map.polygon(&rect).unwrap();
        assert!(polygon.contains(&[8., 3.]));
        assert!(polygon.contains(&[8., 15.]));
        assert!(polygon.len() > 4);
        assert_eq!(map.bounds([0., 0., 16., 12.]).unwrap(), [0., 0., 16., 15.]);
        assert!(map.inverse([-1., 0.]).is_none());
        assert!(map.covers([-1., 0., 16., 12.]).is_err());
    }
    #[test]
    fn strict_grid_preset_identity_fold_overlap_and_work_limits_fail_closed() {
        let grid = Grid::identity([0., 0., 16., 12.]).unwrap();
        assert!(parse(&json!({"op":"transform","warp":grid}))
            .unwrap()
            .is_none());
        for value in [
            Value::Null,
            json!({"rect":[0,0,16,12],"preset":"arc","amount":0.3}),
            json!({"rect":[0,0,16,12],"preset":"unknown","amount":0}),
            json!({"rect":[0,0,16,12],"points":grid.points,"unknown":true}),
        ] {
            assert!(parse(&json!({"op":"transform","warp":value})).is_err());
        }
        for change in 0..4 {
            let mut bad = grid.clone();
            match change {
                0 => bad.points[4] = [24., 6.],
                1 => bad.points[8] = bad.points[0],
                2 => bad.points[4] = [8., 1e-12],
                _ => bad.points[4] = [f64::NAN, 0.],
            }
            assert!(bad.prepare().is_err());
        }
        for name in ["arc", "bulge", "flag"] {
            for amount in [-0.25, 0., 0.25] {
                Grid::preset(grid.rect, name, amount).unwrap();
            }
        }
        assert!(work_budget(8192, 8192, 1, 1, 0).is_err());
        assert!(work_budget(32, 24, 2, 16, 8).is_ok());
        assert!(work_budget(1, 1, usize::MAX, 16, 8192).is_err());
        let map = Grid::preset(grid.rect, "arc", 0.25)
            .unwrap()
            .prepare()
            .unwrap();
        assert!(map.polygon(&vec![[0., 0.]; 8192]).is_err());
    }
    #[test]
    fn retained_inverse_outside_is_never_reinterpreted_as_an_earlier_valid_sample() {
        let grid = Grid::preset([0., 0., 16., 12.], "arc", 0.25).unwrap();
        let stage = Stage {
            before: Map::affine_matrix([1., 0., 0., 1., -10., -10.]).matrix(),
            grid,
        };
        let placement = Placement::new(&[stage], Map::IDENTITY).unwrap();
        // A premature [-2,-2] sentinel could become [8,8] after inverse before.
        assert!(placement.source([-2., -2.]).is_none());
        assert_eq!(placement.sample([-2., -2.]), [-2., -2.]);
        let p = [14., 12.];
        let q = [4., 2. + 1.5];
        let restored = placement.source(q).unwrap();
        for a in 0..2 {
            assert!((restored[a] - p[a]).abs() < 1e-10);
        }
        let stages = vec![
            Stage {
                before: Map::IDENTITY.matrix(),
                grid: Grid::identity([0., 0., 16., 12.]).unwrap()
            };
            17
        ];
        assert!(Placement::new(&stages, Map::IDENTITY).is_err());
    }
    #[test]
    fn shallow_nonparallel_seam_and_target_edge_crossings_are_not_discarded() {
        let mut grid = Grid::identity([0., 0., 16., 12.]).unwrap();
        grid.points[4][1] += 1.5;
        let map = grid.prepare().unwrap();
        let p = [1., 6. - 1e-11];
        let q = [13., 6. + 2e-11];
        let polygon = map.polygon(&[p, q, [8., 10.]]).unwrap();
        let t = (6. - p[1]) / (q[1] - p[1]);
        let x = p[0] + (q[0] - p[0]) * t;
        assert!(polygon
            .iter()
            .any(|point| (point[0] - x).abs() < 1e-8
                && (point[1] - (6. + 1.5 * x / 8.)).abs() < 1e-8));
        assert!(intersects(
            [0., 0.],
            [10000., 0.],
            [0., -1e-7],
            [10000., 1e-7]
        ));
        let mut thin = Grid::identity([0., 0., 16., 12.]).unwrap();
        for point in &mut thin.points {
            point[0] *= 1e-9;
        }
        assert!(thin.prepare().is_err());
    }
}
