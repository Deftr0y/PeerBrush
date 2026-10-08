//! Editable native text/vector definitions supplement standard raster channels.
use crate::{
    engine::{id, Document, Layer},
    raster::{check_size, Raster},
};
use ab_glyph::{point, Font, FontRef, PxScale, ScaleFont};
use serde::{Deserialize, Serialize};
use serde_json::Value;

fn identity() -> [f32; 6] {
    [1., 0., 0., 1., 0., 0.]
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Source {
    pub width: u32,
    pub height: u32,
    #[serde(default = "identity")]
    pub matrix: [f32; 6],
    pub content: Content,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Content {
    Text {
        text: String,
        font: String,
        size: f32,
        line_height: f32,
        align: String,
        color: [u16; 4],
    },
    Shape {
        shape: String,
        bounds: [f32; 4],
        points: Vec<[f32; 2]>,
        closed: bool,
        fill: [u16; 4],
        stroke: [u16; 4],
        stroke_width: f32,
    },
}
pub fn font(name: &str) -> Result<FontRef<'static>, String> {
    let data: &'static [u8] = match name {
        "regular" => include_bytes!("../assets/fonts/UbuntuSans-Regular.ttf"),
        "medium" => include_bytes!("../assets/fonts/UbuntuSans-Medium.ttf"),
        "semibold" => include_bytes!("../assets/fonts/UbuntuSans-SemiBold.ttf"),
        _ => return Err("Choose the bundled regular, medium or semibold font".into()),
    };
    FontRef::try_from_slice(data).map_err(|_| "Bundled font is invalid".into())
}
impl Source {
    pub fn validate(&self) -> Result<(), String> {
        check_size(self.width, self.height)?;
        if u64::from(self.width) * u64::from(self.height) > 4_000_000 {
            return Err("Editable text/shape frames are limited to four million pixels".into());
        }
        let [a, b, c, d, _, _] = self.matrix;
        if self
            .matrix
            .iter()
            .any(|v| !v.is_finite() || v.abs() > 100000.)
            || (a * d - b * c).abs() < 0.000001
        {
            return Err("Invalid editable-source transform".into());
        }
        match &self.content {
            Content::Text {
                text,
                font: name,
                size,
                line_height,
                align,
                ..
            } => {
                if text.len() > 16384
                    || text.chars().count() > 4096
                    || text.lines().count() > 96
                    || !size.is_finite()
                    || !(1. ..=1024.).contains(size)
                    || !line_height.is_finite()
                    || !(0.5..=4.).contains(line_height)
                    || !["left", "center", "right"].contains(&align.as_str())
                {
                    return Err("Invalid text length, size, line spacing or alignment".into());
                }
                let f = font(name)?;
                for ch in text.chars().filter(|c| *c != '\n') {
                    // This deterministic glyph renderer does not perform shaping or bidi.
                    if ch.is_control()
                        || f.glyph_id(ch).0 == 0
                        || matches!(ch as u32,0x0300..=0x036f|0x0590..=0x08ff|0x0900..=0x0fff|0x200c..=0x200f|0x202a..=0x202e|0x2066..=0x2069)
                    {
                        return Err("This text needs an unsupported glyph or script shaping. Use supported plain text or explicitly rasterize it externally.".into());
                    }
                }
            }
            Content::Shape {
                shape,
                bounds,
                points,
                stroke_width,
                ..
            } => {
                if !["rectangle", "ellipse", "path"].contains(&shape.as_str())
                    || bounds.iter().any(|v| !v.is_finite() || v.abs() > 100000.)
                    || bounds[0] >= bounds[2]
                    || bounds[1] >= bounds[3]
                    || !stroke_width.is_finite()
                    || !(0. ..=256.).contains(stroke_width)
                    || points.len() > 128
                    || points
                        .iter()
                        .flatten()
                        .any(|v| !v.is_finite() || v.abs() > 100000.)
                    || (shape == "path" && points.len() < 2)
                {
                    return Err("Invalid vector geometry or stroke; paths need 2–128 points".into());
                }
                if shape == "path"
                    && u64::from(self.width) * u64::from(self.height) * points.len() as u64
                        > 32_000_000
                {
                    return Err("Path geometry exceeds the bounded rendering budget; use a smaller frame or fewer points".into());
                }
            }
        }
        Ok(())
    }
    pub fn label(&self) -> &'static str {
        match self.content {
            Content::Text { .. } => "Text",
            Content::Shape { .. } => "Vector shape",
        }
    }
    pub fn render(&self, w: u32, h: u32, depth: u16) -> Result<Raster, String> {
        self.validate()?;
        check_size(w, h)?;
        if ![8, 16].contains(&depth) {
            return Err("Editable projections require 8 or 16 bit channels".into());
        }
        let mut base = Raster::new_depth(self.width, self.height, depth);
        match &self.content {
            Content::Text {
                text,
                font: name,
                size,
                line_height,
                align,
                color,
            } => {
                let f = font(name)?;
                let scaled = f.as_scaled(PxScale::from(*size));
                let mut work = 0u64;
                for (row, line) in text.split('\n').enumerate() {
                    let mut advance = 0.;
                    let mut previous = None;
                    for ch in line.chars() {
                        let g = scaled.glyph_id(ch);
                        if let Some(p) = previous {
                            advance += scaled.kern(p, g);
                        }
                        advance += scaled.h_advance(g);
                        previous = Some(g);
                    }
                    let start = match align.as_str() {
                        "center" => (self.width as f32 - advance) * 0.5,
                        "right" => self.width as f32 - advance,
                        _ => 0.,
                    };
                    let mut x = start;
                    let mut previous = None;
                    let y = scaled.ascent() + row as f32 * scaled.height() * line_height;
                    for ch in line.chars() {
                        let gid = scaled.glyph_id(ch);
                        if let Some(p) = previous {
                            x += scaled.kern(p, gid);
                        }
                        let g = gid.with_scale_and_position(*size, point(x, y));
                        if let Some(outline) = f.outline_glyph(g) {
                            let bounds = outline.px_bounds();
                            work += bounds.width().ceil() as u64 * bounds.height().ceil() as u64;
                            if work > 32_000_000 {
                                return Err("Text outlines exceed the rendering budget".into());
                            }
                            if bounds.max.x >= 0.
                                && bounds.max.y >= 0.
                                && bounds.min.x < self.width as f32
                                && bounds.min.y < self.height as f32
                            {
                                outline.draw(|xx, yy, coverage| {
                                    let px = bounds.min.x as i32 + xx as i32;
                                    let py = bounds.min.y as i32 + yy as i32;
                                    let mut p = *color;
                                    p[3] = (f32::from(p[3]) * coverage).round() as u16;
                                    if p[3] > 0 {
                                        base.set16(
                                            px,
                                            py,
                                            crate::raster::blend16(
                                                base.get16(px, py),
                                                p,
                                                1.,
                                                "normal",
                                            ),
                                        );
                                    }
                                });
                            }
                        }
                        x += scaled.h_advance(gid);
                        previous = Some(gid);
                    }
                }
            }
            Content::Shape {
                shape,
                bounds,
                points,
                closed,
                fill,
                stroke,
                stroke_width,
            } => {
                let half = *stroke_width * 0.5;
                let path_bounds = if shape == "path" {
                    [
                        points.iter().map(|p| p[0]).fold(f32::INFINITY, f32::min),
                        points.iter().map(|p| p[1]).fold(f32::INFINITY, f32::min),
                        points
                            .iter()
                            .map(|p| p[0])
                            .fold(f32::NEG_INFINITY, f32::max),
                        points
                            .iter()
                            .map(|p| p[1])
                            .fold(f32::NEG_INFINITY, f32::max),
                    ]
                } else {
                    *bounds
                };
                let left = (path_bounds[0] - half - 1.).floor().max(0.) as u32;
                let top = (path_bounds[1] - half - 1.).floor().max(0.) as u32;
                let right = (path_bounds[2] + half + 1.)
                    .ceil()
                    .min(self.width as f32)
                    .max(0.) as u32;
                let bottom = (path_bounds[3] + half + 1.)
                    .ceil()
                    .min(self.height as f32)
                    .max(0.) as u32;
                for y in top..bottom {
                    for x in left..right {
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
                                    let vertices = if shape == "path" {
                                        points.as_slice()
                                    } else {
                                        &rect
                                    };
                                    let closed = shape != "path" || *closed;
                                    let inside =
                                        closed && crate::selection::contains(vertices, p[0], p[1]);
                                    let count = vertices.len() - usize::from(!closed);
                                    let distance = (0..count)
                                        .map(|i| {
                                            segment_distance(
                                                p,
                                                vertices[i],
                                                vertices[(i + 1) % vertices.len()],
                                            )
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
                        if p[3] > 0 {
                            base.set16(x as i32, y as i32, p);
                        }
                    }
                }
            }
        }
        if self.matrix == identity() && (w, h) == (self.width, self.height) {
            return Ok(base);
        }
        let [a, b, c, d, tx, ty] = self.matrix;
        let det = a * d - b * c;
        Ok(crate::transform::resample(&base, w, h, None, |x, y| {
            let xx = x as f32 + 0.5 - tx;
            let yy = y as f32 + 0.5 - ty;
            [
                (d * xx - c * yy) / det - 0.5,
                (-b * xx + a * yy) / det - 0.5,
            ]
        }))
    }
    pub fn prepend(&mut self, n: [f32; 6]) {
        let [a, b, c, d, x, y] = self.matrix;
        let [e, f, g, h, u, v] = n;
        self.matrix = [
            e * a + g * b,
            f * a + h * b,
            e * c + g * d,
            f * c + h * d,
            e * x + g * y + u,
            f * x + h * y + v,
        ];
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
pub fn parse(c: &Value) -> Result<Source, String> {
    serde_json::from_value(c["source"].clone()).map_err(|e| format!("Invalid editable source: {e}"))
}
pub fn add(doc: &mut Document, c: &Value) -> Result<(), String> {
    if doc.layers.len() >= 100 {
        return Err("Layer limit reached".into());
    }
    let source = parse(c)?;
    source.validate()?;
    let parent = c["parent"].as_str();
    if c.get("parent")
        .is_some_and(|v| !v.is_null() && !v.is_string())
    {
        return Err("Parent must be a folder ID or null".into());
    }
    if let Some(p) = parent {
        crate::placement::unlocked(doc, p)?;
        if !doc.layers.iter().any(|l| l.id == p && l.kind == "group") {
            return Err("Parent must be a folder".into());
        }
    }
    let mut layer = Layer::new(
        c["name"].as_str().unwrap_or(source.label()),
        "paint",
        source.width,
        source.height,
    );
    for (key, coordinate) in [("x", &mut layer.x), ("y", &mut layer.y)] {
        if let Some(v) = c.get(key) {
            *coordinate = v
                .as_i64()
                .filter(|v| (-100000..=100000).contains(v))
                .ok_or("Source origin must be an integer within coordinate limits")?
                as i32;
        }
    }
    layer.parent = parent.map(String::from);
    layer.pixels = source.render(source.width, source.height, doc.bit_depth)?;
    layer.source = Some(source);
    doc.layers.insert(0, layer);
    Ok(())
}
pub fn update(layer: &mut Layer, c: &Value) -> Result<(), String> {
    let old = layer
        .source
        .as_ref()
        .ok_or("This layer has no editable text/vector source")?;
    let source = parse(c)?;
    if (source.width, source.height) != (old.width, old.height) {
        return Err("Use image resize to change source dimensions; edit text/vector properties within their existing frame".into());
    }
    let pixels = source.render(layer.pixels.width, layer.pixels.height, layer.pixels.depth)?;
    layer.source = Some(source);
    layer.pixels = pixels;
    layer.effect_key = id();
    Ok(())
}
pub fn transformed(before: &Layer, after: &mut Layer, c: &Value) -> Result<(), String> {
    let Some(mut source) = before.source.clone() else {
        return Ok(());
    };
    if c["op"] == "move" {
        return Ok(());
    }
    let n = |key: &str, default: f32| c[key].as_f64().map(|v| v as f32).unwrap_or(default);
    let (mut sin, mut cos) = n("angle", 0.).to_radians().sin_cos();
    if sin.abs() < 0.000001 {
        sin = 0.;
    }
    if cos.abs() < 0.000001 {
        cos = 0.;
    }
    let (sx, sy) = (n("scale_x", 1.), n("scale_y", 1.));
    let px = c["pivot"][0]
        .as_f64()
        .map(|v| v as f32)
        .unwrap_or(before.x as f32 + before.pixels.width as f32 * 0.5);
    let py = c["pivot"][1]
        .as_f64()
        .map(|v| v as f32)
        .unwrap_or(before.y as f32 + before.pixels.height as f32 * 0.5);
    let [a, b, cc, d] = [cos * sx, sin * sx, -sin * sy, cos * sy];
    source.prepend([
        a,
        b,
        cc,
        d,
        px + a * (before.x as f32 - px) + cc * (before.y as f32 - py) - after.x as f32,
        py + b * (before.x as f32 - px) + d * (before.y as f32 - py) - after.y as f32,
    ]);
    after.pixels = source.render(after.pixels.width, after.pixels.height, after.pixels.depth)?;
    after.source = Some(source);
    Ok(())
}
