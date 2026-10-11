//! Editable cubic paths. Source coordinates stay native; commands use document pixels.
use crate::{
    engine::{id, Document, Layer},
    source::{Content, Source},
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Anchor {
    pub point: [f32; 2],
    #[serde(default)]
    pub in_handle: Option<[f32; 2]>,
    #[serde(default)]
    pub out_handle: Option<[f32; 2]>,
    #[serde(default)]
    pub smooth: bool,
}
fn lerp(a: [f32; 2], b: [f32; 2]) -> [f32; 2] {
    [(a[0] + b[0]) * 0.5, (a[1] + b[1]) * 0.5]
}
fn distance(p: [f32; 2], a: [f32; 2], b: [f32; 2]) -> f32 {
    let d = [b[0] - a[0], b[1] - a[1]];
    let length = d[0] * d[0] + d[1] * d[1];
    let t = if length > 0. {
        ((p[0] - a[0]) * d[0] + (p[1] - a[1]) * d[1]) / length
    } else {
        0.
    }
    .clamp(0., 1.);
    ((p[0] - a[0] - t * d[0]).powi(2) + (p[1] - a[1] - t * d[1]).powi(2)).sqrt()
}
fn curve(p: [[f32; 2]; 4], depth: u8, out: &mut Vec<[f32; 2]>) -> Result<(), String> {
    if out.len() >= 4096 {
        return Err("Path exceeds 4096 flattened vertices; simplify its curves".into());
    }
    if distance(p[1], p[0], p[3]).max(distance(p[2], p[0], p[3])) <= 0.125 {
        out.push(p[3]);
        return Ok(());
    }
    if depth >= 18 {
        return Err("Curve cannot be resolved within the path subdivision budget".into());
    }
    let a = lerp(p[0], p[1]);
    let b = lerp(p[1], p[2]);
    let c = lerp(p[2], p[3]);
    let d = lerp(a, b);
    let e = lerp(b, c);
    let f = lerp(d, e);
    curve([p[0], a, d, f], depth + 1, out)?;
    curve([f, e, c, p[3]], depth + 1, out)
}
pub fn flatten(nodes: &[Anchor], closed: bool) -> Result<Vec<[f32; 2]>, String> {
    if nodes.len() > 128
        || (closed && nodes.len() < 2)
        || nodes.iter().any(|n| {
            std::iter::once(&n.point)
                .chain(n.in_handle.iter())
                .chain(n.out_handle.iter())
                .flatten()
                .any(|v| !v.is_finite() || v.abs() > 100000.)
        })
    {
        return Err("Paths require at most 128 finite anchors/handles; closed paths need at least two anchors".into());
    }
    let mut out = nodes.first().map(|n| vec![n.point]).unwrap_or_default();
    let count = nodes.len().saturating_sub(usize::from(!closed));
    for i in 0..count {
        let a = &nodes[i];
        let b = &nodes[(i + 1) % nodes.len()];
        curve(
            [
                a.point,
                a.out_handle.unwrap_or(a.point),
                b.in_handle.unwrap_or(b.point),
                b.point,
            ],
            0,
            &mut out,
        )?;
    }
    if closed && out.len() > 1 && out.last() == out.first() {
        out.pop();
    }
    Ok(out)
}
pub fn document_point(layer: &Layer, p: [f32; 2]) -> [f32; 2] {
    let m = layer
        .source
        .as_ref()
        .map(|s| s.matrix)
        .unwrap_or([1., 0., 0., 1., 0., 0.]);
    [
        m[0] * p[0] + m[2] * p[1] + m[4] + layer.x as f32,
        m[1] * p[0] + m[3] * p[1] + m[5] + layer.y as f32,
    ]
}
fn local(layer: &Layer, p: [f32; 2]) -> Result<[f32; 2], String> {
    if p.iter().any(|v| !v.is_finite() || v.abs() > 100000.) {
        return Err(
            "Path coordinates must be finite document pixels within coordinate limits".into(),
        );
    }
    let m = layer
        .source
        .as_ref()
        .ok_or("Layer has no path source")?
        .matrix;
    let x = p[0] - layer.x as f32 - m[4];
    let y = p[1] - layer.y as f32 - m[5];
    let det = m[0] * m[3] - m[1] * m[2];
    Ok([(m[3] * x - m[2] * y) / det, (-m[1] * x + m[0] * y) / det])
}
fn point(c: &Value, key: &str) -> Result<[f32; 2], String> {
    serde_json::from_value(c[key].clone())
        .map_err(|_| format!("{key} needs [x,y] in document pixels"))
}
pub fn create(doc: &mut Document, c: &Value) -> Result<(), String> {
    let frame = if c.get("frame").is_some() {
        crate::engine::rect(&c["frame"]).ok_or("Invalid path frame")?
    } else {
        [0, 0, doc.width as i32, doc.height as i32]
    };
    if frame[0] >= frame[2] || frame[1] >= frame[3] {
        return Err("Path frame must have positive dimensions".into());
    }
    let mut nodes: Vec<Anchor> = serde_json::from_value(c["nodes"].clone())
        .map_err(|_| "Path creation needs a nodes array in document pixels")?;
    for n in &mut nodes {
        for p in std::iter::once(&mut n.point)
            .chain(n.in_handle.iter_mut())
            .chain(n.out_handle.iter_mut())
        {
            p[0] -= frame[0] as f32;
            p[1] -= frame[1] as f32;
        }
    }
    let source = Source {
        width: (frame[2] - frame[0]) as u32,
        height: (frame[3] - frame[1]) as u32,
        matrix: [1., 0., 0., 1., 0., 0.],
        content: Content::Bezier {
            nodes,
            closed: bool_field(c, "closed", false)?,
            fill: color(c, "fill", [0; 4])?,
            stroke: color(c, "stroke", [59881, 21588, 8224, 65535])?,
            stroke_width: number(c, "stroke_width", 2.)?,
        },
    };
    crate::source::add(
        doc,
        &json!({"op":"source.add","source":source,"name":c["name"].as_str().unwrap_or("Pen path"),"x":frame[0],"y":frame[1],"parent":c.get("parent").unwrap_or(&Value::Null)}),
    )
}
fn bool_field(c: &Value, key: &str, default: bool) -> Result<bool, String> {
    c.get(key)
        .map(|v| v.as_bool().ok_or_else(|| format!("{key} must be boolean")))
        .unwrap_or(Ok(default))
}
fn number(c: &Value, key: &str, default: f32) -> Result<f32, String> {
    c.get(key)
        .map(|v| {
            v.as_f64()
                .filter(|v| v.is_finite())
                .map(|v| v as f32)
                .ok_or_else(|| format!("{key} must be finite"))
        })
        .unwrap_or(Ok(default))
}
fn color(c: &Value, key: &str, default: [u16; 4]) -> Result<[u16; 4], String> {
    c.get(key)
        .map(|v| {
            serde_json::from_value(v.clone())
                .map_err(|_| format!("{key} needs four native RGBA16 values"))
        })
        .unwrap_or(Ok(default))
}
pub fn update(layer: &mut Layer, c: &Value) -> Result<(), String> {
    let mut source = layer.source.clone().ok_or("Choose an editable Pen path")?;
    let Content::Bezier {
        nodes,
        closed,
        fill,
        stroke,
        stroke_width,
    } = &mut source.content
    else {
        return Err("Choose an editable Pen path".into());
    };
    let op = c["op"].as_str().unwrap_or("");
    if op == "path.update" {
        *closed = bool_field(c, "closed", *closed)?;
        *fill = color(c, "fill", *fill)?;
        *stroke = color(c, "stroke", *stroke)?;
        *stroke_width = number(c, "stroke_width", *stroke_width)?;
    } else if op == "path.node.add" {
        if *closed && c.get("index").is_none() {
            return Err("Reopen the path before adding anchors".into());
        }
        let index = c
            .get("index")
            .map(|v| {
                v.as_u64()
                    .filter(|v| *v <= nodes.len() as u64)
                    .map(|v| v as usize)
                    .ok_or("Invalid anchor index")
            })
            .transpose()?
            .unwrap_or(nodes.len());
        if index > nodes.len() {
            return Err("Invalid anchor index".into());
        }
        let handle = |key: &str| -> Result<Option<[f32; 2]>, String> {
            if c.get(key).is_some_and(|v| !v.is_null()) {
                Ok(Some(local(layer, point(c, key)?)?))
            } else {
                Ok(None)
            }
        };
        nodes.insert(
            index,
            Anchor {
                point: local(layer, point(c, "point")?)?,
                in_handle: handle("in_handle")?,
                out_handle: handle("out_handle")?,
                smooth: bool_field(c, "smooth", false)?,
            },
        );
    } else {
        let index = c["index"]
            .as_u64()
            .filter(|v| *v < nodes.len() as u64)
            .ok_or("Invalid anchor index")? as usize;
        if op == "path.node.split" {
            if !*closed && index + 1 >= nodes.len() {
                return Err("Choose an existing segment".into());
            }
            let t = number(c, "t", 0.5)?;
            if !(0.001..=0.999).contains(&t) {
                return Err("Split t must be within 0.001–0.999".into());
            }
            let next = (index + 1) % nodes.len();
            let a = &nodes[index];
            let b = &nodes[next];
            let mix =
                |a: [f32; 2], b: [f32; 2]| [a[0] * (1. - t) + b[0] * t, a[1] * (1. - t) + b[1] * t];
            let p = mix(a.point, a.out_handle.unwrap_or(a.point));
            let q = mix(
                a.out_handle.unwrap_or(a.point),
                b.in_handle.unwrap_or(b.point),
            );
            let r = mix(b.in_handle.unwrap_or(b.point), b.point);
            let s = mix(p, q);
            let u = mix(q, r);
            let v = mix(s, u);
            nodes[index].out_handle = Some(p);
            nodes[next].in_handle = Some(r);
            nodes.insert(
                index + 1,
                Anchor {
                    point: v,
                    in_handle: Some(s),
                    out_handle: Some(u),
                    smooth: true,
                },
            );
        } else if op == "path.node.delete" {
            nodes.remove(index);
            if nodes.len() < 2 {
                *closed = false;
            }
        } else if op == "path.node.move" {
            let p = local(layer, point(c, "point")?)?;
            let n = &mut nodes[index];
            let delta = [p[0] - n.point[0], p[1] - n.point[1]];
            for h in n.in_handle.iter_mut().chain(n.out_handle.iter_mut()) {
                h[0] += delta[0];
                h[1] += delta[1];
            }
            n.point = p;
        } else if op == "path.handle.move" {
            let p = local(layer, point(c, "point")?)?;
            let n = &mut nodes[index];
            let (handle, other) = match c["handle"].as_str() {
                Some("in") => (&mut n.in_handle, &mut n.out_handle),
                Some("out") => (&mut n.out_handle, &mut n.in_handle),
                _ => return Err("Handle must be in or out".into()),
            };
            *handle = Some(p);
            if n.smooth {
                let d = [p[0] - n.point[0], p[1] - n.point[1]];
                let length = d[0].hypot(d[1]);
                let old = other
                    .map(|p| (p[0] - n.point[0]).hypot(p[1] - n.point[1]))
                    .unwrap_or(length);
                *other = Some(if length > 0.000001 {
                    [
                        n.point[0] - d[0] * old / length,
                        n.point[1] - d[1] * old / length,
                    ]
                } else {
                    n.point
                });
            }
        } else if op == "path.node.convert" {
            let smooth = c["smooth"].as_bool().ok_or("smooth must be boolean")?;
            let previous = index
                .checked_sub(1)
                .or_else(|| (*closed).then_some(nodes.len() - 1));
            let next = (index + 1 < nodes.len())
                .then_some(index + 1)
                .or_else(|| (*closed).then_some(0));
            let a = previous
                .map(|i| nodes[i].point)
                .unwrap_or(nodes[index].point);
            let b = next.map(|i| nodes[i].point).unwrap_or(nodes[index].point);
            let n = &mut nodes[index];
            n.smooth = smooth;
            if smooth {
                let d = [(b[0] - a[0]) / 6., (b[1] - a[1]) / 6.];
                n.in_handle = Some([n.point[0] - d[0], n.point[1] - d[1]]);
                n.out_handle = Some([n.point[0] + d[0], n.point[1] + d[1]]);
            } else if bool_field(c, "reset_handles", false)? {
                n.in_handle = None;
                n.out_handle = None;
            }
        } else {
            return Err("Unknown path operation".into());
        }
    }
    layer.pixels = source.render(layer.pixels.width, layer.pixels.height, layer.pixels.depth)?;
    layer.source = Some(source);
    layer.effect_key = id();
    Ok(())
}
pub fn polygon(layer: &Layer) -> Result<Vec<[f32; 2]>, String> {
    let Some(Source {
        content:
            Content::Bezier {
                nodes,
                closed: true,
                ..
            },
        ..
    }) = &layer.source
    else {
        return Err("Close an editable Pen path before converting its area".into());
    };
    let points = flatten(nodes, true)?
        .into_iter()
        .map(|p| document_point(layer, p))
        .collect::<Vec<_>>();
    crate::selection::bounds(&points)?;
    Ok(points)
}
pub fn observe(layer: &Layer) -> Value {
    let Some(Source {
        content: Content::Bezier { nodes, .. },
        ..
    }) = &layer.source
    else {
        return Value::Null;
    };
    json!(nodes.iter().enumerate().map(|(index,n)|json!({"index":index,"point":document_point(layer,n.point),"in_handle":n.in_handle.map(|p|document_point(layer,p)),"out_handle":n.out_handle.map(|p|document_point(layer,p)),"smooth":n.smooth})).collect::<Vec<_>>())
}
/// Bounded shared hit testing; segment insertion uses exact de Casteljau geometry.
pub fn nearest_segment(layer: &Layer, point: [f32; 2], tolerance: f32) -> Option<(usize, f32)> {
    let Some(Source {
        content: Content::Bezier { nodes, closed, .. },
        ..
    }) = &layer.source
    else {
        return None;
    };
    let count = nodes.len().saturating_sub(usize::from(!closed));
    let mut best = (tolerance * tolerance, None);
    for i in 0..count {
        let a = &nodes[i];
        let b = &nodes[(i + 1) % nodes.len()];
        let p = [
            a.point,
            a.out_handle.unwrap_or(a.point),
            b.in_handle.unwrap_or(b.point),
            b.point,
        ]
        .map(|p| document_point(layer, p));
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
fn selection_command(doc: &Document, c: &Value) -> Result<Value, String> {
    let target = c["path"].as_str().ok_or("Supply the source path ID")?;
    crate::placement::unlocked(doc, target)?;
    let layer = doc
        .layers
        .iter()
        .find(|l| l.id == target)
        .ok_or("Unknown source path")?;
    Ok(
        json!({"op":"selection","kind":"polygon","polygon":polygon(layer)?,"mode":c.get("mode").unwrap_or(&json!("replace")),"feather":c.get("feather").unwrap_or(&json!(0.))}),
    )
}
pub(crate) fn to_selection(doc: &mut Document, c: &Value) -> Result<(), String> {
    let command = selection_command(doc, c)?;
    crate::selection::apply(doc, &command)
}
pub(crate) fn to_mask(doc: &mut Document, index: usize, c: &Value) -> Result<(), String> {
    let mut command = selection_command(doc, c)?;
    command["mode"] = json!("replace");
    let before = (
        doc.selection,
        doc.selection_polygon.clone(),
        doc.selection_coverage.clone(),
        doc.selection_previous.clone(),
        doc.wand_source.clone(),
    );
    crate::selection::apply(doc, &command)?;
    let result = crate::selection::masks::apply(
        doc,
        index,
        &json!({"op":"mask.from_selection","mode":c.get("mask_mode").unwrap_or(&json!("replace"))}),
    );
    (
        doc.selection,
        doc.selection_polygon,
        doc.selection_coverage,
        doc.selection_previous,
        doc.wand_source,
    ) = before;
    result
}
