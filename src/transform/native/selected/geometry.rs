use crate::{
    large_document::{Reservation, ResidentBudget},
    projective::Map,
};
use serde_json::Value;

pub(crate) struct Placement {
    pub area: [i32; 4],
    pub bounds: [i32; 4],
    pub polygon: Vec<[f32; 2]>,
    pub noop: bool,
    delta: [f32; 2],
    scale: [f32; 2],
    pivot: [f32; 2],
    color_trig: [f32; 2],
    scalar_trig: [f32; 2],
    shear: [f32; 2],
    projective: Option<Map>,
    warp: Option<crate::warp::CommandMap>,
    _charge: Reservation,
}
impl Placement {
    pub(crate) fn for_selection(
        selection: &super::input::Selection<'_>,
        command: &Value,
        pool: &ResidentBudget,
    ) -> Result<Self, String> {
        Self::from_polygon(selection.bounds, selection.polygon(), command, pool)
    }
    pub(crate) fn new(
        area: [i32; 4],
        command: &Value,
        pool: &ResidentBudget,
    ) -> Result<Self, String> {
        Self::from_polygon(area, None, command, pool)
    }
    fn from_polygon(
        area: [i32; 4],
        points: Option<&[[f32; 2]]>,
        command: &Value,
        pool: &ResidentBudget,
    ) -> Result<Self, String> {
        let charge = pool
            .reserve_bytes(1024 * 1024, 1024 * 1024)
            .map_err(|e| e.to_string())?;
        let projective = crate::projective::combined(command)?;
        let warp = crate::warp::parse(command)?
            .map(|_| crate::warp::CommandMap::new(command, [0., 0.]))
            .transpose()?;
        if let Some(map) = &warp {
            map.covers([
                f64::from(area[0]) - 1.,
                f64::from(area[1]) - 1.,
                f64::from(area[2]) + 1.,
                f64::from(area[3]) + 1.,
            ])?;
            map.bounds(area.map(f64::from))?;
        } else if let Some(map) = projective {
            map.bounds(area.map(f64::from))?;
        }
        let shear = crate::transform::selected_shear(command)?;
        let n =
            |key: &str, default: f32| command[key].as_f64().unwrap_or(f64::from(default)) as f32;
        let moving = command["op"] == "move";
        let delta = if moving {
            [n("dx", 0.).round(), n("dy", 0.).round()]
        } else {
            crate::transform::translation(command)?.map(|v| v as f32)
        };
        let angle = if moving { 0. } else { n("angle", 0.) };
        let scale = if moving {
            [1., 1.]
        } else {
            let (a, b) = crate::transform::scales(command)?;
            [a as f32, b as f32]
        };
        let pivot = [
            command["pivot"][0]
                .as_f64()
                .unwrap_or((f64::from(area[0]) + f64::from(area[2])) / 2.) as f32,
            command["pivot"][1]
                .as_f64()
                .unwrap_or((f64::from(area[1]) + f64::from(area[3])) / 2.) as f32,
        ];
        if !delta
            .into_iter()
            .chain([angle])
            .chain(pivot)
            .all(|v| v.is_finite() && v.abs() <= 100000.)
            || scale.iter().any(|v| !(0.05..=20.).contains(&v.abs()))
        {
            return Err("Invalid selection transform".into());
        }
        let (sin, cos) = angle.to_radians().sin_cos();
        let color_trig = [
            if sin.abs() < 0.000001 { 0. } else { sin },
            if cos.abs() < 0.000001 { 0. } else { cos },
        ];
        let scalar_trig = if shear == [0., 0.] {
            [sin, cos]
        } else {
            color_trig
        };
        let rectangle = [
            [area[0] as f32, area[1] as f32],
            [area[2] as f32, area[1] as f32],
            [area[2] as f32, area[3] as f32],
            [area[0] as f32, area[3] as f32],
        ];
        let points = points.unwrap_or(&rectangle);
        let polygon = if let Some(map) = &warp {
            map.polygon(points)?
        } else if let Some(map) = projective {
            points
                .iter()
                .map(|p| map.point(p.map(f64::from)).map(|p| p.map(|v| v as f32)))
                .collect::<Result<Vec<_>, _>>()?
        } else {
            points
                .iter()
                .map(|p| {
                    crate::transform::selected_point(*p, pivot, delta, scale, color_trig, shear)
                })
                .collect()
        };
        if polygon
            .capacity()
            .checked_mul(8)
            .is_none_or(|n| n > 256 * 1024)
        {
            return Err("Selected geometry exceeds admitted placement".into());
        }
        let bounds = crate::selection::bounds(&polygon)?;
        let noop = projective.is_none()
            && warp.is_none()
            && delta == [0., 0.]
            && angle.rem_euclid(360.) == 0.
            && scale == [1., 1.]
            && shear == [0., 0.];
        Ok(Self {
            area,
            bounds,
            polygon,
            noop,
            delta,
            scale,
            pivot,
            color_trig,
            scalar_trig,
            shear,
            projective,
            warp,
            _charge: charge,
        })
    }
    pub(super) fn sampler(&self, origin: [i32; 2], scalar: bool) -> Result<Sampler<'_>, String> {
        let inverse = self
            .projective
            .map(|map| {
                Map::affine_matrix([
                    1.,
                    0.,
                    0.,
                    1.,
                    -f64::from(origin[0]) - 0.5,
                    -f64::from(origin[1]) - 0.5,
                ])
                .compose(map.inverse()?)
            })
            .transpose()?;
        Ok(Sampler {
            placement: self,
            origin,
            scalar,
            inverse,
        })
    }
}
pub(super) struct Sampler<'a> {
    placement: &'a Placement,
    origin: [i32; 2],
    scalar: bool,
    inverse: Option<Map>,
}
impl Sampler<'_> {
    pub(super) fn sample(&self, x: i32, y: i32) -> [f32; 2] {
        let p = self.placement;
        if let Some(map) = &p.warp {
            return map.sample(
                [f64::from(x) + 0.5, f64::from(y) + 0.5],
                self.origin.map(f64::from),
            );
        }
        if let Some(map) = self.inverse {
            return map.sample([f64::from(x) + 0.5, f64::from(y) + 0.5]);
        }
        let [sin, cos] = if self.scalar {
            p.scalar_trig
        } else {
            p.color_trig
        };
        let rx = x as f32 + 0.5 - p.pivot[0] - p.delta[0];
        let ry = y as f32 + 0.5 - p.pivot[1] - p.delta[1];
        let mut u = (rx * cos + ry * sin) / p.scale[0];
        let mut v = (-rx * sin + ry * cos) / p.scale[1];
        if p.shear != [0., 0.] {
            v -= p.shear[1] * u;
            u -= p.shear[0] * v;
        }
        [
            u + p.pivot[0] - self.origin[0] as f32 - 0.5,
            v + p.pivot[1] - self.origin[1] as f32 - 0.5,
        ]
    }
}
