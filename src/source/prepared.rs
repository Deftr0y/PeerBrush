//! Explicit native source generation. Definition metadata remains separate from
//! provisional pixels, and no returned region grants document edit authority.
use crate::{
    engine::scene::{Content36, Source36},
    large_document::{PinnedBytes, Reservation, ResidentBudget, ScratchBytes},
    raster::{
        backed::{Region, MAX_REGION_PIXELS},
        project16, Pixel16,
    },
};
use std::sync::Arc;
#[path = "prepared/shape.rs"]
mod shape;
#[path = "prepared/text.rs"]
mod text;
#[path = "prepared/validation.rs"]
mod validation;
pub(crate) use validation::validate_definition;
#[cfg(test)]
#[path = "prepared/tests.rs"]
mod tests;

type Check<'a> = &'a mut dyn FnMut() -> Result<(), String>;
enum Kind<'a> {
    Shape(shape::Shape<'a>),
    Bezier {
        points: Vec<[f32; 2]>,
        closed: bool,
        fill: Pixel16,
        stroke: Pixel16,
        width: f32,
    },
    Text(text::TextPlan),
    Svg(crate::svg::prepared::PreparedSvg),
}
pub(crate) struct PreparedSource<'a> {
    definition: &'a Source36,
    matrix: [f32; 6],
    kind: Kind<'a>,
    pool: ResidentBudget,
    _charge: Reservation,
}
impl<'a> PreparedSource<'a> {
    pub(crate) fn uses_budget(&self, pool: &ResidentBudget) -> bool {
        self.pool.same_pool(pool)
    }
    pub(crate) fn is_known_zero(&self) -> bool {
        match &self.kind {
            Kind::Shape(s) => s.fill[3] == 0 && s.stroke[3] == 0,
            Kind::Bezier {
                points,
                fill,
                stroke,
                ..
            } => points.is_empty() || (fill[3] == 0 && stroke[3] == 0),
            Kind::Text(t) => t.is_known_zero(),
            Kind::Svg(_) => false,
        }
    }
    pub(crate) fn region_pixels(&self) -> usize {
        let vertices = match &self.kind {
            Kind::Shape(s) => s.points.len().max(4),
            Kind::Bezier { points, .. } => points.len().max(4),
            _ => return MAX_REGION_PIXELS,
        };
        (32_000_000 / (vertices * 128)).clamp(1, MAX_REGION_PIXELS)
    }
    pub(crate) fn new(
        source: &'a Source36,
        font: Option<Arc<crate::fonts::prepared::FontOwner>>,
        pool: &ResidentBudget,
        check: Check<'_>,
    ) -> Result<Self, String> {
        Self::new_transformed(source, source.matrix, font, pool, check)
    }
    pub(crate) fn new_transformed(
        source: &'a Source36,
        matrix: [f32; 6],
        font: Option<Arc<crate::fonts::prepared::FontOwner>>,
        pool: &ResidentBudget,
        check: Check<'_>,
    ) -> Result<Self, String> {
        validate_definition(source, pool, check)?;
        validation::matrix(matrix)?;
        // Metadata, flattened points, glyph placement and parser working state
        // are bounded independently of a16000-square pixel plane.
        let charge = pool
            .reserve_bytes(4 * 1024 * 1024, 4 * 1024 * 1024)
            .map_err(|e| e.to_string())?;
        let kind = match &source.content {
            Content36::Shape {
                shape,
                bounds,
                points,
                closed,
                fill,
                stroke,
                stroke_width,
            } => Kind::Shape(shape::Shape {
                kind: shape,
                bounds: *bounds,
                points,
                closed: *closed,
                fill: *fill,
                stroke: *stroke,
                stroke_width: *stroke_width,
            }),
            Content36::Bezier {
                nodes,
                closed,
                fill,
                stroke,
                stroke_width,
            } => Kind::Bezier {
                points: crate::path::flatten(nodes, *closed)?,
                closed: *closed,
                fill: *fill,
                stroke: *stroke,
                width: *stroke_width,
            },
            Content36::Text {
                font: reference, ..
            } => {
                let font =
                    font.ok_or("Editable text requires its admitted immutable font snapshot")?;
                if !font.uses_budget(pool) || !font.matches(reference) {
                    return Err("Editable text font identity or resident pool differs".into());
                }
                Kind::Text(text::TextPlan::new(source, font, pool, check)?)
            }
            Content36::Svg { svg } => Kind::Svg(crate::svg::prepared::PreparedSvg::new(
                svg,
                [source.width, source.height],
                pool,
                check,
            )?),
        };
        check()?;
        Ok(Self {
            definition: source,
            matrix,
            kind,
            pool: pool.clone(),
            _charge: charge,
        })
    }
    pub(crate) fn render_region(
        &self,
        frame: [u32; 2],
        region: Region,
        depth: u16,
        pool: &ResidentBudget,
        check: Check<'_>,
    ) -> Result<NativeRegion, String> {
        if !self.pool.same_pool(pool)
            || ![8, 16].contains(&depth)
            || frame.iter().any(|v| *v == 0 || *v > 16000)
        {
            return Err("Native source output frame, depth or resource pool differs".into());
        }
        let b = region.bounds();
        if b[0] < 0 || b[1] < 0 || b[2] > frame[0] as i32 || b[3] > frame[1] as i32 {
            return Err("Native source region is outside its captured output frame".into());
        }
        check()?;
        if let Kind::Text(text) = &self.kind {
            return self.render_text(text, frame, region, depth, pool, check);
        }
        if let Kind::Svg(svg) = &self.kind {
            return svg.render(frame, self.matrix, region, depth, pool, check);
        }
        let shape = match &self.kind {
            Kind::Shape(s) => *s,
            Kind::Bezier {
                points,
                closed,
                fill,
                stroke,
                width,
            } => shape::Shape {
                kind: "path",
                bounds: [
                    0.,
                    0.,
                    self.definition.width as f32,
                    self.definition.height as f32,
                ],
                points,
                closed: *closed,
                fill: *fill,
                stroke: *stroke,
                stroke_width: *width,
            },
            _ => return Err("Native source region kernel was not prepared".into()),
        };
        let work = region
            .pixels()
            .checked_mul(shape.points.len().max(4))
            .and_then(|n| n.checked_mul(128))
            .ok_or("Native source work overflow")?;
        if work > 32_000_000 {
            return Err(
                "Native source region exceeds bounded analytic work; request a smaller region"
                    .into(),
            );
        }
        let stride = if depth == 16 { 8 } else { 4 };
        let length = region.pixels() * stride;
        let mut output = ScratchBytes::new(pool, length, length).map_err(|e| e.to_string())?;
        let source_frame = [self.definition.width, self.definition.height];
        let [a, bb, c, d, tx, ty] = self.matrix;
        let det = a * d - bb * c;
        let direct = self.matrix == [1., 0., 0., 1., 0., 0.] && frame == source_frame;
        let mut at = 0;
        for y in b[1]..b[3] {
            check()?;
            for x in b[0]..b[2] {
                let pixel = if direct {
                    shape.pixel(source_frame, x, y, depth)
                } else {
                    let xx = x as f32 + 0.5 - tx;
                    let yy = y as f32 + 0.5 - ty;
                    let u = (d * xx - c * yy) / det - 0.5;
                    let v = (-bb * xx + a * yy) / det - 0.5;
                    sample(u, v, source_frame, depth, |sx, sy| {
                        Ok(shape.pixel(source_frame, sx, sy, depth))
                    })?
                };
                if depth == 16 {
                    for word in pixel {
                        output[at..at + 2].copy_from_slice(&word.to_be_bytes());
                        at += 2;
                    }
                } else {
                    output[at..at + 4].copy_from_slice(&pixel.map(project16));
                    at += 4;
                }
            }
        }
        check()?;
        Ok(NativeRegion {
            region,
            depth,
            bytes: output.freeze(),
        })
    }
    fn mapped(&self, x: i32, y: i32) -> [f32; 2] {
        let [a, b, c, d, tx, ty] = self.matrix;
        let det = a * d - b * c;
        let xx = x as f32 + 0.5 - tx;
        let yy = y as f32 + 0.5 - ty;
        [
            (d * xx - c * yy) / det - 0.5,
            (-b * xx + a * yy) / det - 0.5,
        ]
    }
    fn render_text(
        &self,
        text: &text::TextPlan,
        frame: [u32; 2],
        region: Region,
        depth: u16,
        pool: &ResidentBudget,
        check: Check<'_>,
    ) -> Result<NativeRegion, String> {
        let source_frame = [self.definition.width, self.definition.height];
        if self.matrix == [1., 0., 0., 1., 0., 0.] && frame == source_frame {
            return text.render(region, depth, pool, check);
        }
        let b = region.bounds();
        let mut support = [source_frame[0] as i32, source_frame[1] as i32, 0, 0];
        // Scan actual f32 mapping, including integer fast-path support. This
        // does not infer a footprint from ideal real-number corner arithmetic.
        for y in b[1]..b[3] {
            check()?;
            for x in b[0]..b[2] {
                let [u, v] = self.mapped(x, y);
                if !u.is_finite() || !v.is_finite() {
                    return Err("Editable source mapping is nonfinite".into());
                }
                if u < -1. || v < -1. || u >= source_frame[0] as f32 || v >= source_frame[1] as f32
                {
                    continue;
                }
                let n = if u.fract() == 0. && v.fract() == 0. {
                    1
                } else {
                    2
                };
                let l = u.floor() as i32;
                let t = v.floor() as i32;
                support = [
                    support[0].min(l.max(0)),
                    support[1].min(t.max(0)),
                    support[2].max((l + n).min(source_frame[0] as i32)),
                    support[3].max((t + n).min(source_frame[1] as i32)),
                ];
            }
        }
        let stride = if depth == 16 { 8 } else { 4 };
        let len = region.pixels() * stride;
        let mut output = ScratchBytes::new(pool, len, len).map_err(|e| e.to_string())?;
        if support[0] < support[2] && support[1] < support[3] {
            let pixels = (support[2] - support[0]) as usize * (support[3] - support[1]) as usize;
            if pixels > MAX_REGION_PIXELS {
                // Split output, not source geometry. The exact scan repeats at
                // most18 bounded levels; every child uses global output coords.
                let (mut first, mut second) = (b, b);
                if b[2] - b[0] >= b[3] - b[1] && b[2] - b[0] > 1 {
                    let middle = b[0] + (b[2] - b[0]) / 2;
                    first[2] = middle;
                    second[0] = middle;
                } else if b[3] - b[1] > 1 {
                    let middle = b[1] + (b[3] - b[1]) / 2;
                    first[3] = middle;
                    second[1] = middle;
                } else {
                    return Err("A single text sample exceeds prepared source support".into());
                }
                for bounds in [first, second] {
                    let child = Region::new(bounds).map_err(|e| e.to_string())?;
                    let rendered = self.render_text(text, frame, child, depth, pool, check)?;
                    let width = (bounds[2] - bounds[0]) as usize;
                    for (row, bytes) in rendered.bytes().chunks_exact(width * stride).enumerate() {
                        check()?;
                        let at = (((bounds[1] - b[1]) as usize + row) * (b[2] - b[0]) as usize
                            + (bounds[0] - b[0]) as usize)
                            * stride;
                        output[at..at + bytes.len()].copy_from_slice(bytes);
                    }
                }
                check()?;
                return Ok(NativeRegion {
                    region,
                    depth,
                    bytes: output.freeze(),
                });
            }
            let input = Region::new(support)
                .map_err(|_| "Mapped text footprint differs from explicit regional admission")?;
            let base = text.render(input, depth, pool, check)?;
            let mut at = 0;
            for y in b[1]..b[3] {
                check()?;
                for x in b[0]..b[2] {
                    let [u, v] = self.mapped(x, y);
                    let p = sample(u, v, source_frame, depth, |sx, sy| {
                        if sx < 0
                            || sy < 0
                            || sx >= source_frame[0] as i32
                            || sy >= source_frame[1] as i32
                        {
                            Ok([0; 4])
                        } else {
                            base.pixel16(sx, sy)
                        }
                    })?;
                    if depth == 16 {
                        for value in p {
                            output[at..at + 2].copy_from_slice(&value.to_be_bytes());
                            at += 2;
                        }
                    } else {
                        output[at..at + 4].copy_from_slice(&p.map(project16));
                        at += 4;
                    }
                }
            }
        }
        check()?;
        Ok(NativeRegion {
            region,
            depth,
            bytes: output.freeze(),
        })
    }
}

#[derive(Clone)]
pub(crate) struct NativeRegion {
    region: Region,
    depth: u16,
    bytes: PinnedBytes,
}
impl NativeRegion {
    pub(crate) fn from_bytes(
        region: Region,
        depth: u16,
        bytes: PinnedBytes,
    ) -> Result<Self, String> {
        let stride = match depth {
            8 => 4,
            16 => 8,
            _ => return Err("Native source region depth differs".into()),
        };
        if bytes.len() != region.pixels() * stride {
            return Err("Native source region bytes do not match geometry".into());
        }
        Ok(Self {
            region,
            depth,
            bytes,
        })
    }
    pub(crate) fn bounds(&self) -> [i32; 4] {
        self.region.bounds()
    }
    pub(crate) fn depth(&self) -> u16 {
        self.depth
    }
    pub(crate) fn bytes(&self) -> &[u8] {
        &self.bytes
    }
    pub(crate) fn uses_budget(&self, pool: &ResidentBudget) -> bool {
        self.bytes.uses_budget(pool)
    }
    pub(crate) fn pixel16(&self, x: i32, y: i32) -> Result<Pixel16, String> {
        let b = self.bounds();
        if x < b[0] || y < b[1] || x >= b[2] || y >= b[3] {
            return Err("Native source sample is outside its prepared footprint".into());
        }
        let stride = if self.depth == 16 { 8 } else { 4 };
        let at = ((y - b[1]) as usize * (b[2] - b[0]) as usize + (x - b[0]) as usize) * stride;
        Ok(std::array::from_fn(|c| {
            if self.depth == 16 {
                u16::from_be_bytes(self.bytes[at + c * 2..at + c * 2 + 2].try_into().unwrap())
            } else {
                u16::from(self.bytes[at + c]) * 257
            }
        }))
    }
}
fn sample(
    x: f32,
    y: f32,
    frame: [u32; 2],
    depth: u16,
    mut get: impl FnMut(i32, i32) -> Result<Pixel16, String>,
) -> Result<Pixel16, String> {
    if !x.is_finite() || !y.is_finite() {
        return Err("Editable source mapping is nonfinite".into());
    }
    // Metadata proves all four interpolation neighbours are physical exterior.
    if x < -1. || y < -1. || x >= frame[0] as f32 || y >= frame[1] as f32 {
        return Ok([0; 4]);
    }
    if x.fract() == 0. && y.fract() == 0. {
        return get(x as i32, y as i32);
    }
    let ix = x.floor() as i32;
    let iy = y.floor() as i32;
    Ok(if depth == 16 {
        let fx = f64::from(x - x.floor());
        let fy = f64::from(y - y.floor());
        let mut sum = [0f64; 4];
        for (dx, dy, w) in [
            (0, 0, (1. - fx) * (1. - fy)),
            (1, 0, fx * (1. - fy)),
            (0, 1, (1. - fx) * fy),
            (1, 1, fx * fy),
        ] {
            let p = get(ix + dx, iy + dy)?;
            let a = f64::from(p[3]) / 65535.;
            for c in 0..3 {
                sum[c] += f64::from(p[c]) * a * w;
            }
            sum[3] += a * w;
        }
        if sum[3] <= 0. {
            [0; 4]
        } else {
            [
                (sum[0] / sum[3]).round().clamp(0., 65535.) as u16,
                (sum[1] / sum[3]).round().clamp(0., 65535.) as u16,
                (sum[2] / sum[3]).round().clamp(0., 65535.) as u16,
                (sum[3] * 65535.).round().clamp(0., 65535.) as u16,
            ]
        }
    } else {
        let fx = x - x.floor();
        let fy = y - y.floor();
        let mut sum = [0f32; 4];
        for (dx, dy, w) in [
            (0, 0, (1. - fx) * (1. - fy)),
            (1, 0, fx * (1. - fy)),
            (0, 1, (1. - fx) * fy),
            (1, 1, fx * fy),
        ] {
            let p = get(ix + dx, iy + dy)?.map(project16);
            let a = f32::from(p[3]) / 255.;
            for c in 0..3 {
                sum[c] += f32::from(p[c]) * a * w;
            }
            sum[3] += a * w;
        }
        if sum[3] <= 0.00001 {
            [0; 4]
        } else {
            [
                u16::from((sum[0] / sum[3]).round() as u8) * 257,
                u16::from((sum[1] / sum[3]).round() as u8) * 257,
                u16::from((sum[2] / sum[3]).round() as u8) * 257,
                u16::from((sum[3] * 255.).round() as u8) * 257,
            ]
        }
    })
}
