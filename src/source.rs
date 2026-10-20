//! Editable native text/vector definitions supplement standard raster channels.
use crate::{
    engine::{id, Document, Layer},
    raster::{check_size, Raster},
};
use ab_glyph::{point, Font, FontArc, FontRef, PxScale, ScaleFont};
use serde::{Deserialize, Serialize};
use serde_json::Value;

fn identity() -> [f32; 6] {
    [1., 0., 0., 1., 0., 0.]
}
fn zero(value: &f32) -> bool {
    *value == 0.
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
    Bezier {
        nodes: Vec<crate::path::Anchor>,
        closed: bool,
        fill: [u16; 4],
        stroke: [u16; 4],
        stroke_width: f32,
    },
    Text {
        text: String,
        font: crate::fonts::TextFont,
        size: f32,
        line_height: f32,
        #[serde(default, skip_serializing_if = "zero")]
        letter_spacing: f32,
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
    Svg {
        svg: String,
    },
}
pub fn font(name: &str) -> Result<FontRef<'static>, String> {
    crate::fonts::bundled(name).map_err(|error| match error {
        crate::fonts::FontError::InvalidReference => {
            "Choose the bundled regular, medium or semibold font".into()
        }
        _ => "Bundled font is invalid".into(),
    })
}
fn spacing(ch: char) -> bool {
    matches!(
        ch,
        ' ' | '\u{00a0}' | '\u{1680}' | '\u{2000}'
            ..='\u{200a}' | '\u{202f}' | '\u{205f}' | '\u{3000}'
    )
}
fn validate_installed_text(text: &str) -> Result<(), String> {
    use unicode_script::{Script, UnicodeScript};
    for ch in text.chars().filter(|ch| *ch != '\n') {
        let supported = match ch.script() {
            Script::Latin
            | Script::Greek
            | Script::Cyrillic
            | Script::Han
            | Script::Hiragana
            | Script::Katakana
            | Script::Bopomofo
            | Script::Common => true,
            Script::Hangul => matches!(ch, '\u{ac00}'..='\u{d7a3}'),
            _ => false,
        };
        if !supported || unicode_bidi::bidi_class(ch) == unicode_bidi::BidiClass::NSM {
            return Err("This text needs an unsupported glyph or script shaping. Use supported plain text or explicitly rasterize it externally.".into());
        }
    }
    Ok(())
}
fn validate_glyphs(font: &impl Font, text: &str) -> Result<(), String> {
    for ch in text.chars().filter(|ch| *ch != '\n') {
        let glyph = font.glyph_id(ch);
        if glyph.0 == 0 || (!spacing(ch) && font.outline(glyph).is_none()) {
            return Err("This text needs an unsupported glyph or script shaping. Use supported plain text or explicitly rasterize it externally.".into());
        }
    }
    Ok(())
}
fn outline_work(bounds: ab_glyph::Rect, previous: u64) -> Result<u64, String> {
    if [
        bounds.min.x,
        bounds.min.y,
        bounds.max.x,
        bounds.max.y,
        bounds.width(),
        bounds.height(),
    ]
    .iter()
    .any(|value| !value.is_finite())
        || bounds.min.x > bounds.max.x
        || bounds.min.y > bounds.max.y
    {
        return Err("Invalid text outline bounds".into());
    }
    (bounds.width().ceil() as u64)
        .checked_mul(bounds.height().ceil() as u64)
        .and_then(|pixels| previous.checked_add(pixels))
        .filter(|work| *work <= 32_000_000)
        .ok_or_else(|| "Text outlines exceed the rendering budget".into())
}
impl Source {
    pub fn validate(&self) -> Result<(), String> {
        self.validated_font().map(|_| ())
    }
    /// Saved native projections remain usable without an installed font snapshot.
    pub fn validate_structure(&self) -> Result<(), String> {
        self.validate_definition()?;
        if let Content::Text {
            text,
            font: crate::fonts::TextFont::Bundled(name),
            ..
        } = &self.content
        {
            let font = font(name)?;
            if text
                .chars()
                .filter(|ch| *ch != '\n')
                .any(|ch| font.glyph_id(ch).0 == 0)
            {
                return Err("This text needs an unsupported glyph or script shaping. Use supported plain text or explicitly rasterize it externally.".into());
            }
        }
        Ok(())
    }
    fn validated_font(&self) -> Result<Option<FontArc>, String> {
        self.validate_definition()?;
        if let Content::Text { text, font, .. } = &self.content {
            if matches!(font, crate::fonts::TextFont::Installed(_)) {
                validate_installed_text(text)?;
            }
            let font = crate::fonts::snapshot(font).map_err(|error| error.to_string())?;
            validate_glyphs(&font, text)?;
            return Ok(Some(font));
        }
        Ok(None)
    }
    fn validate_definition(&self) -> Result<(), String> {
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
            Content::Bezier {
                nodes,
                closed,
                stroke_width,
                ..
            } => {
                let points = crate::path::flatten(nodes, *closed)?;
                if !stroke_width.is_finite() || !(0. ..=256.).contains(stroke_width) {
                    return Err("Invalid path stroke width".into());
                }
                if !points.is_empty() {
                    let pad = stroke_width * 0.5 + 1.;
                    let w = (points
                        .iter()
                        .map(|p| p[0])
                        .fold(f32::NEG_INFINITY, f32::max)
                        + pad)
                        .min(self.width as f32)
                        .max(0.)
                        - (points.iter().map(|p| p[0]).fold(f32::INFINITY, f32::min) - pad).max(0.);
                    let h = (points
                        .iter()
                        .map(|p| p[1])
                        .fold(f32::NEG_INFINITY, f32::max)
                        + pad)
                        .min(self.height as f32)
                        .max(0.)
                        - (points.iter().map(|p| p[1]).fold(f32::INFINITY, f32::min) - pad).max(0.);
                    if w.max(0.).ceil() as u64 * h.max(0.).ceil() as u64 * points.len() as u64
                        > 32_000_000
                    {
                        return Err("Path exceeds the bounded rendering budget; use a smaller frame or simpler curves".into());
                    }
                }
            }
            Content::Svg { svg } => {
                if crate::svg::dimensions(svg)? != (self.width, self.height) {
                    return Err("SVG viewport must match its editable source frame".into());
                }
            }
            Content::Text {
                text,
                font: name,
                size,
                line_height,
                letter_spacing,
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
                    || !letter_spacing.is_finite()
                    || !(-1024. ..=1024.).contains(letter_spacing)
                    || !["left", "center", "right"].contains(&align.as_str())
                {
                    return Err(
                        "Invalid text length, size, line/letter spacing or alignment".into(),
                    );
                }
                name.validate().map_err(|error| error.to_string())?;
                for ch in text.chars().filter(|c| *c != '\n') {
                    // This deterministic glyph renderer does not perform shaping or bidi.
                    if ch.is_control()
                        || matches!(ch as u32,0x200c..=0x200f|0x202a..=0x202e|0x2066..=0x2069)
                        || (name.bundled_name().is_some()
                            && matches!(ch as u32,0x0300..=0x036f|0x0590..=0x08ff|0x0900..=0x0fff))
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
            Content::Bezier { .. } => "Pen path",
            Content::Text { .. } => "Text",
            Content::Shape { .. } => "Vector shape",
            Content::Svg { .. } => "SVG",
        }
    }
    pub fn render(&self, w: u32, h: u32, depth: u16) -> Result<Raster, String> {
        let font = self.validated_font()?;
        self.render_validated(w, h, depth, font.as_ref())
    }
    fn render_validated(
        &self,
        w: u32,
        h: u32,
        depth: u16,
        font: Option<&FontArc>,
    ) -> Result<Raster, String> {
        check_size(w, h)?;
        if ![8, 16].contains(&depth) {
            return Err("Editable projections require 8 or 16 bit channels".into());
        }
        if let Content::Svg { svg } = &self.content {
            return crate::svg::render(svg, [self.width, self.height], self.matrix, w, h, depth);
        }
        if let Content::Bezier {
            nodes,
            closed,
            fill,
            stroke,
            stroke_width,
        } = &self.content
        {
            let points = crate::path::flatten(nodes, *closed)?;
            if points.is_empty() {
                return Ok(Raster::new_depth(w, h, depth));
            }
            let projected = Source {
                width: self.width,
                height: self.height,
                matrix: self.matrix,
                content: Content::Shape {
                    shape: "path".into(),
                    bounds: [0., 0., self.width as f32, self.height as f32],
                    points,
                    closed: *closed,
                    fill: *fill,
                    stroke: *stroke,
                    stroke_width: *stroke_width,
                },
            };
            return projected.render_validated(w, h, depth, None);
        }
        let mut base = Raster::new_depth(self.width, self.height, depth);
        match &self.content {
            Content::Bezier { .. } => {
                unreachable!("Cubic paths project through validated native geometry")
            }
            Content::Svg { .. } => unreachable!("SVG projects directly from its vector definition"),
            Content::Text {
                text,
                size,
                line_height,
                letter_spacing,
                align,
                color,
                ..
            } => {
                let f = font.ok_or("Validated text font is missing")?;
                let scaled = f.as_scaled(PxScale::from(*size));
                let mut work = 0u64;
                for (row, line) in text.split('\n').enumerate() {
                    let mut advance = 0.;
                    let mut previous = None;
                    for ch in line.chars() {
                        let g = scaled.glyph_id(ch);
                        if let Some(p) = previous {
                            advance += scaled.kern(p, g);
                            advance += *letter_spacing;
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
                            x += *letter_spacing;
                        }
                        let g = gid.with_scale_and_position(*size, point(x, y));
                        if let Some(outline) = f.outline_glyph(g) {
                            let bounds = outline.px_bounds();
                            work = outline_work(bounds, work)?;
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
    source.validate_structure()?;
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
    let mut source = parse(c)?;
    if (source.width, source.height) != (old.width, old.height) {
        if !matches!(old.content, Content::Text { .. })
            || !matches!(source.content, Content::Text { .. })
        {
            return Err("Only editable text fields can change source dimensions".into());
        }
        source.validate_structure()?;
        let [a, b, c, d, tx, ty] = source.matrix.map(f64::from);
        let corners = [
            [0., 0.],
            [f64::from(source.width), 0.],
            [f64::from(source.width), f64::from(source.height)],
            [0., f64::from(source.height)],
        ]
        .map(|[x, y]| [a * x + c * y + tx, b * x + d * y + ty]);
        // Keep the old projection frame so resizing never crops attached mask sources.
        let left = corners.iter().map(|p| p[0]).fold(0., f64::min).floor();
        let top = corners.iter().map(|p| p[1]).fold(0., f64::min).floor();
        let right = corners
            .iter()
            .map(|p| p[0])
            .fold(f64::from(layer.pixels.width), f64::max)
            .ceil();
        let bottom = corners
            .iter()
            .map(|p| p[1])
            .fold(f64::from(layer.pixels.height), f64::max)
            .ceil();
        let x = f64::from(layer.x) + left;
        let y = f64::from(layer.y) + top;
        if [left, top, right, bottom, x, y]
            .iter()
            .any(|v| !v.is_finite() || v.abs() > 100000.)
        {
            return Err("Text field projection exceeds coordinate limits".into());
        }
        let (width, height) = ((right - left) as u32, (bottom - top) as u32);
        check_size(width, height)?;
        crate::transform::work_budget(
            width,
            height,
            1 + layer.mask.as_ref().map_or(0, |mask| mask.steps.len()),
        )?;
        source.matrix[4] = (tx - left) as f32;
        source.matrix[5] = (ty - top) as f32;
        let mut replacement = layer.clone();
        crate::transform::reframe_mask(&mut replacement, x as i32, y as i32, width, height)?;
        if left != 0. || top != 0. {
            for effect in &mut replacement.effects {
                if effect.kind == "liquify" {
                    crate::geometry::map_effect(
                        &effect.kind,
                        &mut effect.settings,
                        [1., 1.],
                        [-left as i32, -top as i32],
                        true,
                    )?;
                }
            }
        }
        replacement.pixels = source.render(width, height, layer.pixels.depth)?;
        replacement.source = Some(source);
        replacement.effect_key = id();
        *layer = replacement;
        return Ok(());
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
    let (sx, sy) = crate::transform::scales(c)?;
    let (sx, sy) = (sx as f32, sy as f32);
    let px = c["pivot"][0]
        .as_f64()
        .map(|v| v as f32)
        .unwrap_or(before.x as f32 + before.pixels.width as f32 * 0.5);
    let py = c["pivot"][1]
        .as_f64()
        .map(|v| v as f32)
        .unwrap_or(before.y as f32 + before.pixels.height as f32 * 0.5);
    let [a, b, cc, d] =
        crate::transform::skewed([cos * sx, sin * sx, -sin * sy, cos * sy].map(f64::from), c)?
            .map(|value| value as f32);
    let [dx, dy] = crate::transform::translation(c)?.map(|value| value as f32);
    let mut delta = [
        a,
        b,
        cc,
        d,
        px + a * (before.x as f32 - px) + cc * (before.y as f32 - py) - after.x as f32,
        py + b * (before.x as f32 - px) + d * (before.y as f32 - py) - after.y as f32,
    ];
    if dx != 0. {
        delta[4] += dx;
    }
    if dy != 0. {
        delta[5] += dy;
    }
    source.prepend(delta);
    after.pixels = source.render(after.pixels.width, after.pixels.height, after.pixels.depth)?;
    after.source = Some(source);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        engine::Engine,
        fonts::{self, FontError, InstalledFont, TextFont},
    };
    use serde_json::json;

    fn reference() -> InstalledFont {
        let digest = ring::digest::digest(
            &ring::digest::SHA256,
            include_bytes!("../assets/fonts/UbuntuSans-Regular.ttf"),
        );
        InstalledFont {
            family: "Synthetic font fixture".into(),
            postscript: "PeerBrushSynthetic-Regular".into(),
            style: "normal".into(),
            weight: 400,
            stretch: 5,
            index: 0,
            sha256: digest
                .as_ref()
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect(),
        }
    }
    fn text(font: TextFont) -> Source {
        Source {
            width: 160,
            height: 80,
            matrix: identity(),
            content: Content::Text {
                text: "AV PeerBrush\nNative text".into(),
                font,
                size: 22.,
                line_height: 1.2,
                letter_spacing: 0.,
                align: "left".into(),
                color: [12501, 23003, 34507, 65535],
            },
        }
    }
    fn state(engine: &Engine) -> Value {
        let history = |entries: &[crate::engine::History]| {
            entries.iter().map(|entry| json!({
            "before":entry.before,"after":entry.after,"actor":entry.actor,"label":entry.label,
            "task":entry.task,"scopes":entry.scopes,"gesture":entry.gesture,"reverted":entry.reverted,
        })).collect::<Vec<_>>()
        };
        json!({"document":engine.doc,"png":engine.doc.export_png().unwrap(),
            "undo":history(&engine.undo),"redo":history(&engine.redo)})
    }
    fn fixture(depth: u16, font: TextFont) -> Engine {
        let mut engine = Engine::new();
        engine.doc = Document::new_depth(200, 120, depth).unwrap();
        let source = text("regular".into());
        let mut layer = Layer::new(
            "Installed text fixture",
            "paint",
            source.width,
            source.height,
        );
        layer.pixels = source.render(source.width, source.height, depth).unwrap();
        layer.source = Some(text(font));
        engine.doc.layers.insert(0, layer);
        engine
    }
    #[test]
    fn installed_render_validates_and_renders_one_exact_owned_snapshot() {
        let reference = reference();
        for depth in [8, 16] {
            let expected = text("regular".into()).render(160, 80, depth).unwrap();
            let guard = fonts::test_fonts(vec![(
                reference.clone(),
                Ok(FontArc::new(font("regular").unwrap())),
            )]);
            let source = text(TextFont::Installed(reference.clone()));
            source.validate_structure().unwrap();
            assert_eq!(guard.requests(), 0);
            let rendered = source.render(160, 80, depth).unwrap();
            assert_eq!(guard.requests(), 1);
            assert_eq!(rendered.depth, depth);
            assert_eq!(rendered.rgba16(), expected.rgba16());
            assert!(rendered.rgba16().chunks_exact(4).any(|pixel| pixel[3] > 0));
            if depth == 16 {
                assert!(rendered.rgba16().iter().any(|value| value % 257 != 0));
            }
            source.validate().unwrap();
            assert_eq!(guard.requests(), 2);
        }
    }
    #[test]
    fn font_errors_preserve_baked_sources_and_never_resolve_during_storage_or_clipboard() {
        let reference = reference();
        for depth in [8, 16] {
            for error in [
                FontError::Loading,
                FontError::Busy,
                FontError::Missing,
                FontError::Changed,
                FontError::Unavailable,
                FontError::Unsupported,
                FontError::Variable,
                FontError::ColorOrBitmap,
            ] {
                let source = text(TextFont::Installed(reference.clone()));
                let engine = fixture(depth, TextFont::Installed(reference.clone()));
                let before = state(&engine);
                let guard = fonts::test_fonts(vec![(reference.clone(), Err(error))]);
                source.validate_structure().unwrap();
                assert_eq!(guard.requests(), 0);
                assert_eq!(source.validate().unwrap_err(), error.to_string());
                assert_eq!(
                    source.render(160, 80, depth).unwrap_err(),
                    error.to_string()
                );
                assert_eq!(guard.requests(), 2);
                crate::psd::validate(&engine.doc).unwrap();
                for bytes in [
                    crate::psd::encode(&engine.doc).unwrap(),
                    crate::psd::encode_psb(&engine.doc).unwrap(),
                ] {
                    let reopened = crate::psd::decode(&bytes).unwrap();
                    assert!(!reopened.read_only);
                    assert_eq!(reopened.bit_depth, depth);
                    assert_eq!(reopened.layers[0].source, engine.doc.layers[0].source);
                    assert_eq!(
                        reopened.layers[0].pixels.rgba16(),
                        engine.doc.layers[0].pixels.rgba16()
                    );
                    assert_eq!(
                        reopened.export_png().unwrap(),
                        engine.doc.export_png().unwrap()
                    );
                }
                let snapshot =
                    crate::layer_clipboard::copy(&engine.doc, &[engine.doc.layers[0].id.clone()])
                        .unwrap();
                let mut target = Document::new_depth(200, 120, depth).unwrap();
                crate::layer_clipboard::paste(&mut target, &snapshot, "").unwrap();
                let pasted = target
                    .layers
                    .iter()
                    .find(|layer| layer.source.is_some())
                    .unwrap();
                assert_eq!(pasted.source, engine.doc.layers[0].source);
                assert_eq!(pasted.pixels.rgba16(), engine.doc.layers[0].pixels.rgba16());
                assert_eq!(guard.requests(), 2);
                assert_eq!(state(&engine), before);
            }
        }
    }
    #[test]
    fn missing_font_rejects_rerendering_atomically_but_keeps_move_masks_effects_and_rasterize() {
        let reference = reference();
        for depth in [8, 16] {
            let mut engine = fixture(depth, TextFont::Installed(reference.clone()));
            let guard = fonts::test_fonts(vec![(reference.clone(), Err(FontError::Missing))]);
            let id = engine.doc.layers[0].id.clone();
            let mut updated = engine.doc.layers[0].source.clone().unwrap();
            if let Content::Text { text, .. } = &mut updated.content {
                *text = "Changed human text".into();
            }
            let mut resized = updated.clone();
            resized.width += 20;
            for command in [
                json!({"op":"source.add","source":updated}),
                json!({"op":"source.update","layer":id,"source":updated}),
                json!({"op":"source.update","layer":id,"source":resized}),
                json!({"op":"transform","layer":id,"angle":17.,"selection_only":false}),
                json!({"op":"transform","layer":id,"angle":17.,"dx":8.,"dy":-4.,"selection_only":false}),
                json!({"op":"transform","layer":id,"scale_x":1.2,"scale_y":1.2,"selection_only":false}),
                json!({"op":"transform","layer":id,"skew_x":15.,"skew_y":-10.,"selection_only":false}),
                json!({"op":"image.resize","width":220,"height":132}),
            ] {
                let before = state(&engine);
                let error = engine
                    .edit(
                        "human",
                        &[command.clone()],
                        None,
                        None,
                        "Unavailable text font",
                    )
                    .unwrap_err();
                assert_eq!(error, FontError::Missing.to_string());
                assert_eq!(state(&engine), before);
                assert!(Engine::preview_edits(engine.doc.clone(), &[command]).is_err());
                assert_eq!(state(&engine), before);
            }
            let requests = guard.requests();
            let words = engine.doc.layers[0].pixels.rgba16();
            let definition = engine.doc.layers[0].source.clone();
            for command in [
                json!({"op":"move","layer":id,"dx":3,"dy":2,"selection_only":false}),
                json!({"op":"effect.add","layer":id,"kind":"invert","settings":{}}),
                json!({"op":"mask.add","layer":id,"value":255}),
                json!({"op":"paint","layer":id,"mask":true,"points":[[30,30]],"radius":5,"color":[0,0,0,255]}),
            ] {
                engine
                    .edit(
                        "human",
                        &[command],
                        None,
                        None,
                        "Retain saved text appearance",
                    )
                    .unwrap();
            }
            assert_eq!(guard.requests(), requests);
            assert_eq!(engine.doc.layers[0].source, definition);
            assert_eq!(engine.doc.layers[0].pixels.rgba16(), words);
            let before = state(&engine);
            engine
                .edit(
                    "human",
                    &[json!({"op":"source.rasterize","layer":id})],
                    None,
                    None,
                    "Explicit rasterize",
                )
                .unwrap();
            assert!(engine.doc.layers[0].source.is_none());
            assert_eq!(engine.doc.layers[0].pixels.rgba16(), words);
            assert_eq!(
                engine.doc.export_png().unwrap(),
                engine.undo.last().unwrap().before.export_png().unwrap()
            );
            engine.undo("human").unwrap();
            assert_eq!(engine.doc.layers[0].source, definition);
            assert_eq!(engine.doc.layers[0].pixels.rgba16(), words);
            assert_eq!(json!(engine.doc.layers), before["document"]["layers"]);
            assert_eq!(guard.requests(), requests);
        }
    }
    #[test]
    fn glyphs_without_outlines_are_rejected_and_spacing_keeps_legacy_advances() {
        let font = font("regular").unwrap();
        assert_ne!(font.glyph_id(' ').0, 0);
        assert!(font.outline(font.glyph_id(' ')).is_none());
        validate_glyphs(&font, "AV PeerBrush\n ").unwrap();
        let (_, empty) = font
            .codepoint_ids()
            .find(|(glyph, ch)| {
                if glyph.0 == 0 || spacing(*ch) || font.outline(*glyph).is_some() {
                    return false;
                }
                let mut candidate = text("regular".into());
                if let Content::Text { text, .. } = &mut candidate.content {
                    *text = ch.to_string();
                }
                candidate.validate_definition().is_ok()
            })
            .expect("Bundled fixture includes a nonspacing empty glyph");
        assert!(validate_glyphs(&font, &empty.to_string()).is_err());
        let mut historical = text("regular".into());
        if let Content::Text { text, .. } = &mut historical.content {
            *text = format!("A{empty}B");
        }
        historical.validate_structure().unwrap();
        assert!(historical.validate().is_err());
        assert!(historical.render(160, 80, 16).is_err());
        for unsupported in ["שלום", "مرحبا", "हिन्दी", "A\u{0301}", "A\tB", "A\u{200d}B"]
        {
            let mut source = text("regular".into());
            if let Content::Text { text, .. } = &mut source.content {
                *text = unsupported.into();
            }
            assert!(source.validate_structure().is_err());
            assert!(source.render(160, 80, 16).is_err());
        }
    }
    #[test]
    fn installed_plain_script_policy_keeps_saved_metadata_and_rejects_unshaped_runs() {
        for supported in ["AV PeerBrush 123!", "caf\u{00e9} \u{03a9}\u{03bc}\u{03ad}\u{03b3}\u{03b1} \u{041f}\u{0440}\u{0438}\u{0432}\u{0435}\u{0442}",
            "\u{6f22}\u{5b57} \u{3042}\u{3044}\u{3046} \u{30ab}\u{30bf}\u{30ab}\u{30ca} \u{3105}\u{3106} \u{d55c}\u{ae00}", "A\nB \u{3000}"] {
            validate_installed_text(supported).unwrap();
        }
        let reference = reference();
        let guard = fonts::test_fonts(vec![(
            reference.clone(),
            Ok(FontArc::new(font("regular").unwrap())),
        )]);
        let mut source = text(TextFont::Installed(reference.clone()));
        for unsupported in [
            "שלום",
            "مرحبا",
            "हिन्दी",
            "A\u{0301}",
            "\u{1000}",
            "\u{aa60}",
            "\u{a9e0}",
            "\u{1780}",
            "\u{1820}",
            "A\u{1ab0}",
            "A\u{1dc0}",
            "A\u{20d0}",
            "A\u{fe20}",
            "\u{0483}",
            "\u{1100}",
            "\u{11a8}",
            "\u{a960}",
            "\u{d7b0}",
            "\u{3131}",
            "\u{ffc2}",
            "\u{0531}",
            "\u{10d0}",
            "\u{fffd}\u{e0100}",
        ] {
            assert!(
                validate_installed_text(unsupported).is_err(),
                "{unsupported:?}"
            );
            if let Content::Text { text, .. } = &mut source.content {
                *text = unsupported.into();
            }
            source.validate_structure().unwrap();
            assert!(source.validate().is_err());
            assert!(source.render(160, 80, 16).is_err());
        }
        assert_eq!(guard.requests(), 0);
        if let Content::Text { text, .. } = &mut source.content {
            *text = "AV caf\u{00e9} \u{03a9} \u{0416}".into();
        }
        validate_glyphs(
            &font("regular").unwrap(),
            "AV caf\u{00e9} \u{03a9} \u{0416}",
        )
        .unwrap();
        source.render(160, 80, 16).unwrap();
        assert_eq!(guard.requests(), 1);
        for depth in [8, 16] {
            let mut engine = fixture(depth, TextFont::Installed(reference.clone()));
            if let Content::Text { text, .. } =
                &mut engine.doc.layers[0].source.as_mut().unwrap().content
            {
                *text = "שלום مرحبا हिन्दी \u{1820} A\u{0301}\u{1ab0}".into();
            }
            for bytes in [
                crate::psd::encode(&engine.doc).unwrap(),
                crate::psd::encode_psb(&engine.doc).unwrap(),
            ] {
                let reopened = crate::psd::decode(&bytes).unwrap();
                assert!(!reopened.read_only);
                assert_eq!(reopened.layers[0].source, engine.doc.layers[0].source);
                assert_eq!(
                    reopened.layers[0].pixels.rgba16(),
                    engine.doc.layers[0].pixels.rgba16()
                );
                assert_eq!(
                    reopened.export_png().unwrap(),
                    engine.doc.export_png().unwrap()
                );
            }
        }
        assert_eq!(guard.requests(), 1);
    }
    #[test]
    fn outline_work_rejects_nonfinite_reversed_overflowing_and_excessive_bounds() {
        let bounds = |width, height| ab_glyph::Rect {
            min: point(0., 0.),
            max: point(width, height),
        };
        assert_eq!(outline_work(bounds(10., 20.), 7).unwrap(), 207);
        assert_eq!(outline_work(bounds(4000., 8000.), 0).unwrap(), 32_000_000);
        assert!(outline_work(bounds(4000., 8000.), 1).is_err());
        assert!(outline_work(bounds(1., 1.), u64::MAX).is_err());
        for (width, height) in [
            (f32::NAN, 1.),
            (1., f32::INFINITY),
            (-1., 1.),
            (f32::MAX, f32::MAX),
            (8192., 8192.),
        ] {
            assert!(outline_work(bounds(width, height), 0).is_err());
        }
    }
}
