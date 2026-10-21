//! Original raster samples and accumulated placement are editable source data.
use crate::{
    engine::{Document, Layer},
    raster::{check_size, Raster},
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Original {
    pub pixels: Box<Raster>,
    pub matrix: [f64; 6],
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub projective: Option<[f64; 9]>,
}
pub const IDENTITY: [f64; 6] = [1., 0., 0., 1., 0., 0.];
fn exact(v: f64) -> f64 {
    if (v - v.round()).abs() < 1e-8 {
        v.round()
    } else {
        v
    }
}
pub fn compose(n: [f64; 6], m: [f64; 6]) -> [f64; 6] {
    let [a, b, c, d, x, y] = m;
    let [e, f, g, h, u, v] = n;
    [
        e * a + g * b,
        f * a + h * b,
        e * c + g * d,
        f * c + h * d,
        e * x + g * y + u,
        f * x + h * y + v,
    ]
    .map(exact)
}
impl Original {
    pub fn from(r: &Raster) -> Self {
        r.retained.as_deref().cloned().unwrap_or_else(|| Self {
            pixels: Box::new(r.clone()),
            matrix: IDENTITY,
            projective: None,
        })
    }
    pub fn validate(&self, depth: u16) -> Result<(), String> {
        if self.pixels.retained.is_some() {
            return Err("Nested transform originals are unsupported".into());
        }
        check_size(self.pixels.width, self.pixels.height)?;
        self.pixels.validate_layout()?;
        let [a, b, c, d, _, _] = self.matrix;
        if self.pixels.depth != depth
            || self.matrix.iter().any(|v| !v.is_finite() || v.abs() > 1e8)
            || (a * d - b * c).abs() < 1e-12
        {
            return Err("Invalid retained transform source or placement".into());
        }
        if self.projective.is_some() {
            self.placement()?.bounds([
                0.,
                0.,
                self.pixels.width as f64,
                self.pixels.height as f64,
            ])?;
        }
        Ok(())
    }
    pub(crate) fn placement(&self) -> Result<crate::projective::Map, String> {
        let affine = crate::projective::Map::affine_matrix(self.matrix);
        self.projective
            .map(crate::projective::Map::from_matrix)
            .transpose()?
            .map_or(Ok(affine), |p| affine.compose(p))
    }
    fn prepend(&mut self, delta: crate::projective::Map) -> Result<(), String> {
        let map = delta.compose(self.placement()?)?;
        if let Some(matrix) = map.as_affine() {
            self.matrix = matrix;
            self.projective = None;
        } else {
            self.matrix = IDENTITY;
            self.projective = Some(map.matrix());
        }
        self.validate(self.pixels.depth)
    }
    pub fn render(mut self, w: u32, h: u32) -> Result<Raster, String> {
        self.matrix = self.matrix.map(exact);
        self.validate(self.pixels.depth)?;
        check_size(w, h)?;
        let [a, b, c, d, tx, ty] = self.matrix;
        let det = a * d - b * c;
        let mut out = if self.pixels.bytes() == 0 {
            Raster::new_depth(w, h, self.pixels.depth)
        } else if self.projective.is_some() {
            let inverse = crate::projective::Map::affine_matrix([1., 0., 0., 1., -0.5, -0.5])
                .compose(self.placement()?.inverse()?)?;
            crate::transform::resample(&self.pixels, w, h, None, |x, y| {
                inverse.sample([x as f64 + 0.5, y as f64 + 0.5])
            })
        } else if self.matrix == IDENTITY && (w, h) == (self.pixels.width, self.pixels.height) {
            (*self.pixels).clone()
        } else {
            crate::transform::resample(&self.pixels, w, h, None, |x, y| {
                let (xx, yy) = (x as f64 + 0.5 - tx, y as f64 + 0.5 - ty);
                [
                    exact((d * xx - c * yy) / det - 0.5) as f32,
                    exact((-b * xx + a * yy) / det - 0.5) as f32,
                ]
            })
        };
        out.retained = Some(Box::new(self));
        Ok(out)
    }
    fn bounds(&self, full: bool) -> Result<[f64; 4], String> {
        let p = if full {
            [0, 0, self.pixels.width as i32, self.pixels.height as i32]
        } else {
            self.pixels.source_bounds().unwrap_or([
                0,
                0,
                self.pixels.width as i32,
                self.pixels.height as i32,
            ])
        };
        if self.projective.is_some() {
            self.validate(self.pixels.depth)?;
            return self.placement()?.bounds(p.map(|v| v as f64));
        }
        let [a, b, c, d, tx, ty] = self.matrix;
        let ps = [[p[0], p[1]], [p[2], p[1]], [p[2], p[3]], [p[0], p[3]]].map(|[x, y]| {
            [
                exact(a * x as f64 + c * y as f64 + tx),
                exact(b * x as f64 + d * y as f64 + ty),
            ]
        });
        Ok([
            ps.iter().map(|p| p[0]).fold(f64::INFINITY, f64::min),
            ps.iter().map(|p| p[1]).fold(f64::INFINITY, f64::min),
            ps.iter().map(|p| p[0]).fold(f64::NEG_INFINITY, f64::max),
            ps.iter().map(|p| p[1]).fold(f64::NEG_INFINITY, f64::max),
        ])
    }
}
pub fn layer(layer: &mut Layer, c: &Value) -> Result<(), String> {
    if c["op"] == "move" {
        return crate::transform::move_layer(layer, c);
    }
    let projective = crate::projective::parse(c)?;
    if projective.is_some() && layer.source.is_some() {
        return Err(
            "Rasterize the editable source at its native precision before a projective transform"
                .into(),
        );
    }
    let n = |key: &str, d: f64| c[key].as_f64().unwrap_or(d);
    let angle = n("angle", 0.);
    let (sx, sy) = crate::transform::scales(c)?;
    if !angle.is_finite() {
        return Err("Transform scale must be 0.05–20 and rotation must be finite".into());
    }
    let (px, py) = (
        c["pivot"][0].as_f64().unwrap_or_else(|| {
            projective
                .map(|p| p.center()[0])
                .unwrap_or(layer.x as f64 + layer.pixels.width as f64 / 2.)
        }),
        c["pivot"][1].as_f64().unwrap_or_else(|| {
            projective
                .map(|p| p.center()[1])
                .unwrap_or(layer.y as f64 + layer.pixels.height as f64 / 2.)
        }),
    );
    if !px.is_finite() || !py.is_finite() || px.abs() > 100000. || py.abs() > 100000. {
        return Err("Invalid transform pivot".into());
    }
    let [dx, dy] = crate::transform::translation(c)?;
    if projective.is_none()
        && angle.rem_euclid(360.) == 0.
        && sx == 1.
        && sy == 1.
        && dx == 0.
        && dy == 0.
        && crate::transform::skewed([1., 0., 0., 1.], c)? == [1., 0., 0., 1.]
    {
        return Ok(());
    }
    let (sin, cos) = angle.to_radians().sin_cos();
    let [a, b, cc, d] = crate::transform::skewed(
        [
            exact(cos) * sx,
            exact(sin) * sx,
            -exact(sin) * sy,
            exact(cos) * sy,
        ],
        c,
    )?;
    let mut delta = [
        a,
        b,
        cc,
        d,
        px + a * (layer.x as f64 - px) + cc * (layer.y as f64 - py),
        py + b * (layer.x as f64 - px) + d * (layer.y as f64 - py),
    ];
    if dx != 0. {
        delta[4] += dx;
    }
    if dy != 0. {
        delta[5] += dy;
    }
    let projective_delta = projective
        .map(|p| {
            crate::projective::affine(c, p.center())?
                .compose(p.map)?
                .compose(crate::projective::Map::affine_matrix([
                    1.,
                    0.,
                    0.,
                    1.,
                    layer.x as f64,
                    layer.y as f64,
                ]))
        })
        .transpose()?;
    let world = ["group", "adjustment"].contains(&layer.kind.as_str());
    if world && layer.mask.is_none() {
        return Ok(());
    }
    let mut color = (!world && layer.source.is_none()).then(|| Original::from(&layer.pixels));
    if layer.kind == "fill" {
        if let Some(color) = &mut color {
            let mut r =
                Raster::new_depth(layer.pixels.width, layer.pixels.height, layer.pixels.depth);
            for y in 0..r.height {
                for x in 0..r.width {
                    r.set(x as i32, y as i32, layer.color);
                }
            }
            color.pixels = Box::new(r);
        }
    }
    if let Some(color) = &mut color {
        if let Some(map) = projective_delta {
            color.prepend(map)?;
        } else {
            color.matrix = compose(delta, color.matrix);
        }
    }
    let mut masks: Vec<_> = layer
        .mask
        .as_ref()
        .map(|m| {
            m.steps
                .iter()
                .map(|s| {
                    let mut r = Original::from(&s.pixels);
                    if let Some(map) = projective_delta {
                        r.prepend(map)?;
                    } else {
                        r.matrix = compose(delta, r.matrix);
                    }
                    Ok(r)
                })
                .collect::<Result<Vec<_>, String>>()
        })
        .transpose()?
        .unwrap_or_default();
    let full = layer.mask.as_ref().is_some_and(|m| {
        m.steps
            .iter()
            .any(|s| s.kind == "paint" && s.pixels.bytes() > 0)
    });
    let mut bounds = color.as_ref().map(|s| s.bounds(full)).transpose()?;
    for (s, o) in layer
        .mask
        .as_ref()
        .into_iter()
        .flat_map(|m| m.steps.iter())
        .zip(&masks)
    {
        if s.kind == "paint" && o.pixels.bytes() > 0 {
            let b = o.bounds(true)?;
            bounds = Some(
                bounds
                    .map(|a| {
                        [
                            a[0].min(b[0]),
                            a[1].min(b[1]),
                            a[2].max(b[2]),
                            a[3].max(b[3]),
                        ]
                    })
                    .unwrap_or(b),
            );
        }
    }
    // Editable definitions retain their complete frame, including future text edits.
    if layer.source.is_some() || bounds.is_none() {
        let mut r = Original::from(&Raster::new_depth(
            layer.pixels.width,
            layer.pixels.height,
            layer.pixels.depth,
        ));
        if let Some(map) = projective_delta {
            r.prepend(map)?;
        } else {
            r.matrix = delta;
        }
        let b = r.bounds(true)?;
        bounds = Some(
            bounds
                .map(|a| {
                    [
                        a[0].min(b[0]),
                        a[1].min(b[1]),
                        a[2].max(b[2]),
                        a[3].max(b[3]),
                    ]
                })
                .unwrap_or(b),
        );
    }
    let mut b = bounds.unwrap();
    if projective_delta.is_none()
        && color.as_ref().is_none_or(|s| s.projective.is_none())
        && masks.iter().all(|s| s.projective.is_none())
    {
        b = b.map(exact);
    }
    let (left, top, right, bottom) = (b[0].floor(), b[1].floor(), b[2].ceil(), b[3].ceil());
    if [left, top, right, bottom]
        .iter()
        .any(|v| !v.is_finite() || v.abs() > 100000.)
    {
        return Err("Transform is outside the coordinate limits".into());
    }
    let (w, h) = ((right - left) as u32, (bottom - top) as u32);
    check_size(w, h)?;
    crate::transform::work_budget(
        w,
        h,
        1 + masks.iter().filter(|o| o.pixels.bytes() > 0).count(),
    )?;
    let local = [1., 0., 0., 1., -left, -top];
    let before = layer.source.is_some().then(|| layer.clone());
    layer.pixels = if let Some(mut o) = color {
        o.matrix = compose(local, o.matrix);
        o.render(w, h)?
    } else {
        Raster::new_depth(w, h, layer.pixels.depth)
    };
    if let Some(m) = &mut layer.mask {
        m.cache_key = crate::engine::id();
        for (s, mut o) in m.steps.iter_mut().zip(masks.drain(..)) {
            o.matrix = compose(local, o.matrix);
            s.pixels = if s.kind == "paint" && o.pixels.bytes() > 0 {
                o.render(w, h)?
            } else {
                Raster::new_depth(w, h, layer.pixels.depth)
            };
        }
    }
    layer.x = left as i32;
    layer.y = top as i32;
    if !world {
        layer.kind = "paint".into();
    }
    if let Some(before) = before {
        crate::source::transformed(&before, layer, c)?;
    }
    Ok(())
}
/// Mask-only placement keeps Color and other mask steps unchanged. Off-frame
/// mask samples remain in the retained original, including through a PSD save.
pub(crate) fn mask(layer: &mut Layer, c: &Value) -> Result<(), String> {
    let projective = crate::projective::combined(c)?;
    let moving = c["op"] == "move";
    let (sx, sy) = if moving {
        (1., 1.)
    } else {
        crate::transform::scales(c)?
    };
    let angle = if moving {
        0.
    } else {
        c["angle"].as_f64().unwrap_or(0.)
    };
    let [dx, dy] = if moving {
        [
            c["dx"].as_f64().unwrap_or(0.).round(),
            c["dy"].as_f64().unwrap_or(0.).round(),
        ]
    } else {
        crate::transform::translation(c)?
    };
    let px = c["pivot"][0]
        .as_f64()
        .unwrap_or(layer.x as f64 + layer.pixels.width as f64 / 2.)
        - layer.x as f64;
    let py = c["pivot"][1]
        .as_f64()
        .unwrap_or(layer.y as f64 + layer.pixels.height as f64 / 2.)
        - layer.y as f64;
    if ![angle, px, py, dx, dy]
        .iter()
        .all(|v| v.is_finite() && v.abs() <= 100000.)
    {
        return Err("Invalid mask transform".into());
    }
    if c["step"].is_null() {
        crate::transform::plain_mask(layer)?;
    }
    let mask = layer.mask.as_mut().ok_or("Layer has no mask")?;
    let indices = mask
        .steps
        .iter()
        .enumerate()
        .filter(|(_, step)| {
            step.kind == "paint" && c["step"].as_str().is_none_or(|id| id == step.id)
        })
        .map(|(index, _)| index)
        .collect::<Vec<_>>();
    if !c["step"].is_null() && indices.is_empty() {
        return Err("Select a paint mask step to transform".into());
    }
    crate::transform::work_budget(layer.pixels.width, layer.pixels.height, indices.len())?;
    let (sin, cos) = angle.to_radians().sin_cos();
    let [a, b, cc, d] = crate::transform::skewed(
        [
            exact(cos) * sx,
            exact(sin) * sx,
            -exact(sin) * sy,
            exact(cos) * sy,
        ],
        c,
    )?;
    for index in indices {
        let mut original = Original::from(&mask.steps[index].pixels);
        if let Some(map) = projective {
            original.prepend(
                crate::projective::Map::affine_matrix([
                    1.,
                    0.,
                    0.,
                    1.,
                    -(layer.x as f64),
                    -(layer.y as f64),
                ])
                .compose(map)?
                .compose(crate::projective::Map::affine_matrix([
                    1.,
                    0.,
                    0.,
                    1.,
                    layer.x as f64,
                    layer.y as f64,
                ]))?,
            )?;
        } else {
            original.matrix = compose(
                [
                    a,
                    b,
                    cc,
                    d,
                    px - a * px - cc * py + dx,
                    py - b * px - d * py + dy,
                ],
                original.matrix,
            );
        }
        mask.steps[index].pixels = original.render(layer.pixels.width, layer.pixels.height)?;
    }
    mask.cache_key = crate::engine::id();
    Ok(())
}
pub fn observe(r: &Raster) -> Value {
    r.retained.as_ref().map(|s| {
        let mut value = json!({"width":s.pixels.width,"height":s.pixels.height,"bit_depth":s.pixels.depth,"matrix":s.matrix,"bytes":s.pixels.bytes()});
        if let Some(map) = s.projective { value["projective"] = json!(map); }
        value
    }).unwrap_or(Value::Null)
}
pub fn has_projective(doc: &Document) -> bool {
    doc.layers.iter().any(|l| {
        l.pixels
            .retained
            .as_ref()
            .is_some_and(|o| o.projective.is_some())
            || l.mask.as_ref().is_some_and(|m| {
                m.steps.iter().any(|s| {
                    s.pixels
                        .retained
                        .as_ref()
                        .is_some_and(|o| o.projective.is_some())
                })
            })
    })
}
pub fn has_originals(doc: &Document) -> bool {
    doc.layers.iter().any(|l| {
        l.pixels.retained.is_some()
            || l.mask
                .as_ref()
                .is_some_and(|m| m.steps.iter().any(|s| s.pixels.retained.is_some()))
    })
}
fn same(a: &Raster, b: &Raster) -> bool {
    // An unshifted storage expansion with identical tiles changes no samples.
    // The retained matrix can legitimately stay identical in that case.
    a.depth == b.depth
        && a.width >= b.width
        && a.height >= b.height
        && a.tiles.len() == b.tiles.len()
        && a.samples16.len() == b.samples16.len()
        && a.tiles.iter().all(|(k, v)| {
            b.tiles
                .get(k)
                .is_some_and(|w| std::sync::Arc::ptr_eq(v, w) || v == w)
        })
        && a.samples16.iter().all(|(k, v)| {
            b.samples16
                .get(k)
                .is_some_and(|w| std::sync::Arc::ptr_eq(v, w) || v == w)
        })
}
pub fn reconcile(before: &Document, after: &mut Document) {
    for l in &mut after.layers {
        let Some(old) = before.layers.iter().find(|o| o.id == l.id) else {
            continue;
        };
        if l.pixels.retained == old.pixels.retained && !same(&l.pixels, &old.pixels) {
            l.pixels.retained = None;
        }
        if let (Some(b), Some(a)) = (&old.mask, &mut l.mask) {
            for s in &mut a.steps {
                if let Some(o) = b.steps.iter().find(|o| o.id == s.id) {
                    if s.pixels.retained == o.pixels.retained && !same(&s.pixels, &o.pixels) {
                        s.pixels.retained = None;
                    }
                }
            }
        }
    }
}
