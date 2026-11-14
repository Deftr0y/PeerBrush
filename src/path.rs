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
    document_point_view(layer.into(), p)
}
pub(crate) fn document_point_view(
    layer: crate::engine::view::LayerView<'_>,
    p: [f32; 2],
) -> [f32; 2] {
    point_in_document(
        layer.origin(),
        layer
            .source()
            .map(|s| s.matrix())
            .unwrap_or([1., 0., 0., 1., 0., 0.]),
        p,
    )
}
pub(crate) fn point_in_document(origin: [i32; 2], m: [f32; 6], p: [f32; 2]) -> [f32; 2] {
    [
        m[0] * p[0] + m[2] * p[1] + m[4] + origin[0] as f32,
        m[1] * p[0] + m[3] * p[1] + m[5] + origin[1] as f32,
    ]
}
fn local(origin: [i32; 2], m: [f32; 6], p: [f32; 2]) -> Result<[f32; 2], String> {
    if p.iter().any(|v| !v.is_finite() || v.abs() > 100000.) {
        return Err(
            "Path coordinates must be finite document pixels within coordinate limits".into(),
        );
    }
    let x = p[0] - origin[0] as f32 - m[4];
    let y = p[1] - origin[1] as f32 - m[5];
    let det = m[0] * m[3] - m[1] * m[2];
    Ok([(m[3] * x - m[2] * y) / det, (-m[1] * x + m[0] * y) / det])
}
fn point(c: &Value, key: &str) -> Result<[f32; 2], String> {
    <[f32; 2]>::deserialize(&c[key]).map_err(|_| format!("{key} needs [x,y] in document pixels"))
}
pub fn create(doc: &mut Document, c: &Value) -> Result<(), String> {
    let frame = creation_frame([doc.width, doc.height], c)?;
    let nodes = Vec::<Anchor>::deserialize(&c["nodes"])
        .map_err(|_| "Path creation needs a nodes array in document pixels")?;
    let source = creation_source(frame, c, nodes)?;
    crate::source::add(
        doc,
        &json!({"op":"source.add","source":source,"name":c["name"].as_str().unwrap_or("Pen path"),"x":frame[0],"y":frame[1],"parent":c.get("parent").unwrap_or(&Value::Null)}),
    )
}
pub(crate) fn creation_frame(canvas: [u32; 2], c: &Value) -> Result<[i32; 4], String> {
    let frame = if c.get("frame").is_some() {
        crate::engine::rect(&c["frame"]).ok_or("Invalid path frame")?
    } else {
        [0, 0, canvas[0] as i32, canvas[1] as i32]
    };
    if frame[0] >= frame[2] || frame[1] >= frame[3] {
        return Err("Path frame must have positive dimensions".into());
    }
    Ok(frame)
}
pub(crate) fn creation_source(
    frame: [i32; 4],
    c: &Value,
    mut nodes: Vec<Anchor>,
) -> Result<Source, String> {
    for n in &mut nodes {
        for p in std::iter::once(&mut n.point)
            .chain(n.in_handle.iter_mut())
            .chain(n.out_handle.iter_mut())
        {
            p[0] -= frame[0] as f32;
            p[1] -= frame[1] as f32;
        }
    }
    Ok(Source {
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
    })
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
            <[u16; 4]>::deserialize(v).map_err(|_| format!("{key} needs four native RGBA16 values"))
        })
        .unwrap_or(Ok(default))
}
pub fn update(layer: &mut Layer, c: &Value) -> Result<(), String> {
    let mut source = layer
        .source
        .as_deref()
        .cloned()
        .ok_or("Choose an editable Pen path")?;
    update_definition(&mut source, [layer.x, layer.y], c)?;
    layer.pixels = source.render(layer.pixels.width, layer.pixels.height, layer.pixels.depth)?;
    layer.source = Some(source.into());
    layer.effect_key = id();
    Ok(())
}
pub(crate) fn update_definition(
    source: &mut Source,
    origin: [i32; 2],
    c: &Value,
) -> Result<(), String> {
    let local = |p| local(origin, source.matrix, p);
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
    if nodes.len() > 128 {
        return Err("Paths require at most 128 anchors".into());
    }
    if matches!(op, "path.node.add" | "path.node.split") {
        if nodes.len() == 128 {
            return Err("Paths require at most 128 anchors".into());
        }
        nodes
            .try_reserve_exact(1)
            .map_err(|_| "Path anchor allocation refused")?;
    }
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
                Ok(Some(local(point(c, key)?)?))
            } else {
                Ok(None)
            }
        };
        nodes.insert(
            index,
            Anchor {
                point: local(point(c, "point")?)?,
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
            let p = local(point(c, "point")?)?;
            let n = &mut nodes[index];
            let delta = [p[0] - n.point[0], p[1] - n.point[1]];
            for h in n.in_handle.iter_mut().chain(n.out_handle.iter_mut()) {
                h[0] += delta[0];
                h[1] += delta[1];
            }
            n.point = p;
        } else if op == "path.handle.move" {
            let p = local(point(c, "point")?)?;
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
    }) = layer.source.as_deref()
    else {
        return Err("Close an editable Pen path before converting its area".into());
    };
    transformed_polygon(
        nodes,
        layer.source.as_ref().unwrap().matrix,
        [layer.x, layer.y],
    )
}
pub(crate) fn transformed_polygon(
    nodes: &[Anchor],
    m: [f32; 6],
    origin: [i32; 2],
) -> Result<Vec<[f32; 2]>, String> {
    let mut points = flatten(nodes, true)?;
    for point in &mut points {
        *point = [
            m[0] * point[0] + m[2] * point[1] + m[4] + origin[0] as f32,
            m[1] * point[0] + m[3] * point[1] + m[5] + origin[1] as f32,
        ];
    }
    crate::selection::bounds(&points)?;
    Ok(points)
}
pub fn observe(layer: &Layer) -> Value {
    let Some(Source {
        content: Content::Bezier { nodes, .. },
        ..
    }) = layer.source.as_deref()
    else {
        return Value::Null;
    };
    json!(nodes.iter().enumerate().map(|(index,n)|json!({"index":index,"point":document_point(layer,n.point),"in_handle":n.in_handle.map(|p|document_point(layer,p)),"out_handle":n.out_handle.map(|p|document_point(layer,p)),"smooth":n.smooth})).collect::<Vec<_>>())
}
/// Bounded shared hit testing; segment insertion uses exact de Casteljau geometry.
pub fn nearest_segment(layer: &Layer, point: [f32; 2], tolerance: f32) -> Option<(usize, f32)> {
    nearest_segment_view(layer.into(), point, tolerance)
}
pub(crate) fn nearest_segment_view(
    layer: crate::engine::view::LayerView<'_>,
    point: [f32; 2],
    tolerance: f32,
) -> Option<(usize, f32)> {
    let (nodes, closed) = layer.source()?.path_nodes()?;
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
        .map(|p| document_point_view(layer, p));
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
/// Bounded path metadata for control handles; artwork stays in the shared renderer.
pub(crate) fn preview_controls(
    doc: crate::engine::view::DocumentView<'_>,
    target: Option<crate::engine::view::LayerView<'_>>,
    commands: &[Value],
) -> Result<Option<([i32; 2], Source)>, String> {
    use crate::engine::{scene::Content36, view::SourceView};
    if commands.len() > 100 {
        return Err("Path preview command limit exceeded".into());
    }
    let mut current = target
        .map(|layer| -> Result<_, String> {
            let source = layer.source().ok_or("Choose an editable Pen path")?;
            let (nodes, _) = source.path_nodes().ok_or("Choose an editable Pen path")?;
            if nodes.len() > 128 {
                return Err("Paths require at most 128 anchors".into());
            }
            let definition = match source {
                SourceView::Resident(source) => source.clone(),
                SourceView::Native(source) => {
                    let Content36::Bezier {
                        nodes,
                        closed,
                        fill,
                        stroke,
                        stroke_width,
                    } = source.definition()
                    else {
                        unreachable!()
                    };
                    Source {
                        width: source.width,
                        height: source.height,
                        matrix: source.matrix,
                        content: Content::Bezier {
                            nodes: nodes.clone(),
                            closed: *closed,
                            fill: *fill,
                            stroke: *stroke,
                            stroke_width: *stroke_width,
                        },
                    }
                }
            };
            Ok((layer.origin(), definition))
        })
        .transpose()?;
    for command in commands {
        if command["document_id"]
            .as_str()
            .is_some_and(|id| id != doc.id())
            || command["source_revision"]
                .as_u64()
                .is_some_and(|revision| revision != doc.revision())
        {
            return Err("Path preview source changed".into());
        }
        if command["op"] == "path.create" && current.is_none() {
            let nodes = command["nodes"]
                .as_array()
                .ok_or("Path creation needs a nodes array")?;
            if nodes.len() > 128 {
                return Err("Paths require at most 128 anchors".into());
            }
            let nodes =
                Vec::<Anchor>::deserialize(&command["nodes"]).map_err(|error| error.to_string())?;
            let frame = creation_frame([doc.width(), doc.height()], command)?;
            current = Some((
                [frame[0], frame[1]],
                creation_source(frame, command, nodes)?,
            ));
        } else {
            let layer = target.ok_or("Choose an editable Pen path")?;
            if command["layer"].as_str() != Some(layer.id()) {
                return Err("Path preview target changed".into());
            }
            let (origin, source) = current.as_mut().ok_or("Choose an editable Pen path")?;
            update_definition(source, *origin, command)?;
        }
        if let Some((_, source)) = &current {
            validate_controls(doc, source)?;
        }
    }
    if commands.is_empty() {
        if let Some((_, source)) = &current {
            validate_controls(doc, source)?;
        }
    }
    Ok(current)
}

fn validate_controls(doc: crate::engine::view::DocumentView<'_>, source: &Source) -> Result<(), String> {
    let Some(native) = doc.native() else {
        return source.validate_structure();
    };
    let Content::Bezier { nodes, closed, stroke_width, .. } = &source.content else {
        return Err("Choose an editable Pen path".into());
    };
    crate::raster::CanvasLimits::Native.check(source.width, source.height)?;
    use crate::source::prepared::validation;
    validation::matrix(source.matrix)?;
    validation::bezier(nodes, *closed, *stroke_width, native.budget())
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

#[cfg(test)]
mod preview_tests {
    use super::*;
    use crate::engine::{view::DocumentView, Engine};

    #[test]
    fn metadata_controls_match_engine_path_commands_at_both_depths() {
        for depth in [8, 16] {
            let mut doc = Document::new_depth(32, 24, depth).unwrap();
            create(&mut doc, &json!({"op":"path.create","nodes":[{"point":[2,3]},{"point":[14,11]},{"point":[20,6]}],
                "stroke":[12345,23457,34569,65535]})).unwrap();
            let layer = doc.layers.first_mut().unwrap();
            layer.x = -3;
            layer.y = 5;
            std::sync::Arc::make_mut(layer.source.as_mut().unwrap()).matrix =
                [1.1, 0.25, -0.1, 0.9, 2., -1.];
            let id = layer.id.clone();
            let before = serde_json::to_value(&doc).unwrap();
            for mut command in [
                json!({"op":"path.node.move","index":1,"point":[17,12]}),
                json!({"op":"path.handle.move","index":0,"handle":"out","point":[5,8]}),
                json!({"op":"path.node.add","point":[22,13]}),
                json!({"op":"path.node.split","index":0,"t":0.4}),
                json!({"op":"path.node.delete","index":1}),
                json!({"op":"path.node.convert","index":1,"smooth":true}),
                json!({"op":"path.update","closed":true,"fill":[12347,23459,34571,45679]}),
            ] {
                command["layer"] = json!(id);
                command["document_id"] = json!(doc.id);
                command["source_revision"] = json!(doc.revision);
                let view = DocumentView::from(&doc);
                let target = view.layers().find(|layer| layer.id() == id).unwrap();
                let (origin, controls) = preview_controls(view, Some(target), &[command.clone()])
                    .unwrap()
                    .unwrap();
                let expected = Engine::preview_edits(doc.clone(), &[command]).unwrap();
                let expected = expected.layers.iter().find(|layer| layer.id == id).unwrap();
                assert_eq!(origin, [expected.x, expected.y]);
                assert_eq!(
                    serde_json::to_value(&controls).unwrap(),
                    serde_json::to_value(&expected.source).unwrap()
                );
                assert_eq!(serde_json::to_value(&doc).unwrap(), before);
            }
            let command = json!({"op":"path.create","frame":[-4,2,20,18],"nodes":[{"point":[5,6],"in_handle":[2,4],"out_handle":[8,8],"smooth":true}]});
            let (origin, controls) = preview_controls((&doc).into(), None, &[command.clone()])
                .unwrap()
                .unwrap();
            let expected = Engine::preview_edits(doc.clone(), &[command]).unwrap();
            let expected = expected.layers.first().unwrap();
            assert_eq!(origin, [expected.x, expected.y]);
            assert_eq!(
                serde_json::to_value(controls).unwrap(),
                serde_json::to_value(&expected.source).unwrap()
            );
        }
    }
    #[test]
    fn control_preview_rejects_stale_targets_invalid_intermediates_and_unbounded_paths() {
        let mut doc = Document::new(32, 24).unwrap();
        create(
            &mut doc,
            &json!({"op":"path.create","nodes":[{"point":[2,3]},{"point":[14,11]}]}),
        )
        .unwrap();
        let target = doc.layers.first().unwrap();
        let command = json!({"op":"path.node.move","layer":target.id,"index":0,"point":[4,5]});
        for field in ["document_id", "source_revision", "layer"] {
            let mut changed = command.clone();
            changed[field] = if field == "source_revision" {
                json!(doc.revision + 1)
            } else {
                json!("other")
            };
            assert!(preview_controls((&doc).into(), Some(target.into()), &[changed]).is_err());
        }
        let invalid = json!({"op":"path.update","layer":target.id,"stroke_width":-1});
        let repaired = json!({"op":"path.update","layer":target.id,"stroke_width":2});
        assert!(
            preview_controls((&doc).into(), Some(target.into()), &[invalid, repaired]).is_err()
        );
        assert!(preview_controls((&doc).into(), Some(target.into()), &vec![command; 101]).is_err());
        let oversized = json!({"op":"path.create","nodes":vec![json!({"point":[1,1]});129]});
        assert!(preview_controls((&doc).into(), None, &[oversized]).is_err());
    }
}
