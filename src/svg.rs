//! Bounded self-contained static SVG sources. Unsupported visuals reject explicitly.
use crate::raster::{check_size, Raster};
use resvg::{tiny_skia, usvg};
use std::{collections::HashMap, str::FromStr};

pub const MARKUP_LIMIT: usize = 512 * 1024;
const NODES: usize = 4096;
const GEOMETRY: usize = 65536;
const SVG_NS: &str = "http://www.w3.org/2000/svg";

fn bounded(values: impl IntoIterator<Item = f64>) -> Result<(), String> {
    if values
        .into_iter()
        .any(|v| !v.is_finite() || v.abs() > 100000.)
    {
        return Err("SVG coordinates exceed supported finite bounds".into());
    }
    Ok(())
}
fn path(value: &str) -> Result<usize, String> {
    use svgtypes::PathSegment::*;
    let mut count = 0;
    for segment in svgtypes::PathParser::from(value) {
        let values: Vec<f64> = match segment.map_err(|e| format!("Invalid SVG path: {e}"))? {
            MoveTo { x, y, .. } | LineTo { x, y, .. } | SmoothQuadratic { x, y, .. } => vec![x, y],
            HorizontalLineTo { x, .. } => vec![x],
            VerticalLineTo { y, .. } => vec![y],
            CurveTo {
                x1,
                y1,
                x2,
                y2,
                x,
                y,
                ..
            } => vec![x1, y1, x2, y2, x, y],
            SmoothCurveTo { x2, y2, x, y, .. } => vec![x2, y2, x, y],
            Quadratic { x1, y1, x, y, .. } => vec![x1, y1, x, y],
            EllipticalArc {
                rx,
                ry,
                x_axis_rotation,
                x,
                y,
                ..
            } => vec![rx, ry, x_axis_rotation, x, y],
            ClosePath { .. } => Vec::new(),
        };
        bounded(values)?;
        count += 1;
        if count > GEOMETRY {
            return Err("SVG path segment limit exceeded".into());
        }
    }
    if count == 0 {
        return Err("SVG paths need valid geometry".into());
    }
    Ok(count)
}
fn numbers(value: &str) -> Result<Vec<f64>, String> {
    let values = svgtypes::NumberListParser::from(value)
        .map(|v| v.map_err(|e| format!("Invalid SVG number list: {e}")))
        .collect::<Result<Vec<_>, _>>()?;
    bounded(values.iter().copied())?;
    if values.len() > GEOMETRY {
        return Err("SVG geometry limit exceeded".into());
    }
    Ok(values)
}
fn local(value: &str) -> Result<&str, String> {
    value
        .trim()
        .strip_prefix('#')
        .filter(|v| !v.is_empty() && !v.chars().any(char::is_whitespace))
        .ok_or_else(|| {
            "SVG references must name a local element; external resources are unsupported".into()
        })
}
fn property<'a>(
    name: &str,
    value: &'a str,
    refs: &mut Vec<(&'a str, &'static str)>,
) -> Result<usize, String> {
    let value = value.trim();
    let invalid = || format!("Invalid or unsupported SVG {name} value");
    match name {
        "id" | "class" | "version" | "role" => {}
        "href" => refs.push((local(value)?, "href")),
        "d" => return path(value),
        "points" => {
            let values = numbers(value)?;
            if values.len() < 4 || values.len() % 2 != 0 {
                return Err(invalid());
            }
            return Ok(values.len() / 2);
        }
        "viewBox" => {
            let values = numbers(value)?;
            if values.len() != 4 || values[2] <= 0. || values[3] <= 0. {
                return Err(invalid());
            }
        }
        "transform" | "gradientTransform" => {
            let t = svgtypes::Transform::from_str(value).map_err(|_| invalid())?;
            bounded([t.a, t.b, t.c, t.d, t.e, t.f])?;
        }
        "fill" | "stroke" => match svgtypes::Paint::from_str(value).map_err(|_| invalid())? {
            svgtypes::Paint::FuncIRI(id, _) => refs.push((id, "paint")),
            svgtypes::Paint::ContextFill | svgtypes::Paint::ContextStroke => return Err(invalid()),
            _ => {}
        },
        "color" | "stop-color" => {
            if !["inherit", "currentColor"].contains(&value) {
                svgtypes::Color::from_str(value).map_err(|_| invalid())?;
            }
        }
        "clip-path" => {
            if !["none", "inherit"].contains(&value) {
                let inner = value
                    .strip_prefix("url(")
                    .and_then(|s| s.strip_suffix(')'))
                    .ok_or_else(invalid)?;
                refs.push((local(inner.trim().trim_matches(['\'', '"']))?, "clip"));
            }
        }
        "opacity" | "fill-opacity" | "stroke-opacity" | "stop-opacity" | "offset" => {
            if value != "inherit" {
                let (number, factor) = value.strip_suffix('%').map_or((value, 1.), |s| (s, 0.01));
                let number = number.parse::<f64>().map_err(|_| invalid())? * factor;
                if !number.is_finite() || !(0. ..=1.).contains(&number) {
                    return Err(invalid());
                }
            }
        }
        "x" | "y" | "width" | "height" | "x1" | "y1" | "x2" | "y2" | "cx" | "cy" | "r" | "rx"
        | "ry" | "fx" | "fy" | "stroke-width" | "stroke-dashoffset" => {
            let length = svgtypes::Length::from_str(value).map_err(|_| invalid())?;
            bounded([length.number])?;
            if matches!(
                name,
                "width" | "height" | "r" | "rx" | "ry" | "stroke-width"
            ) && length.number < 0.
            {
                return Err(invalid());
            }
            if matches!(
                length.unit,
                svgtypes::LengthUnit::Em | svgtypes::LengthUnit::Ex
            ) {
                return Err(invalid());
            }
        }
        "stroke-miterlimit" => {
            let n = value.parse::<f64>().map_err(|_| invalid())?;
            if !n.is_finite() || !(1. ..=1000.).contains(&n) {
                return Err(invalid());
            }
        }
        "stroke-dasharray" => {
            if value != "none" {
                let values = numbers(value)?;
                if values.is_empty() || values.len() > 128 || values.iter().any(|v| *v < 0.) {
                    return Err(invalid());
                }
            }
        }
        "fill-rule" | "clip-rule" => {
            if !["nonzero", "evenodd", "inherit"].contains(&value) {
                return Err(invalid());
            }
        }
        "stroke-linecap" => {
            if !["butt", "round", "square", "inherit"].contains(&value) {
                return Err(invalid());
            }
        }
        "stroke-linejoin" => {
            if !["miter", "round", "bevel", "inherit"].contains(&value) {
                return Err(invalid());
            }
        }
        "display" => {
            if !["none", "inline", "inherit"].contains(&value) {
                return Err(invalid());
            }
        }
        "visibility" => {
            if !["visible", "hidden", "collapse", "inherit"].contains(&value) {
                return Err(invalid());
            }
        }
        "overflow" => {
            if !["visible", "hidden", "inherit"].contains(&value) {
                return Err(invalid());
            }
        }
        "shape-rendering" => {
            if ![
                "auto",
                "optimizeSpeed",
                "crispEdges",
                "geometricPrecision",
                "inherit",
            ]
            .contains(&value)
            {
                return Err(invalid());
            }
        }
        "gradientUnits" | "clipPathUnits" => {
            if !["userSpaceOnUse", "objectBoundingBox"].contains(&value) {
                return Err(invalid());
            }
        }
        "spreadMethod" => {
            if !["pad", "reflect", "repeat"].contains(&value) {
                return Err(invalid());
            }
        }
        "color-interpolation" => {
            if value != "sRGB" {
                return Err(invalid());
            }
        }
        "vector-effect" => {
            if value != "none" {
                return Err(invalid());
            }
        }
        "paint-order" => {
            if !["normal", "fill", "stroke", "fill stroke", "stroke fill"].contains(&value) {
                return Err(invalid());
            }
        }
        "preserveAspectRatio" => {
            svgtypes::AspectRatio::from_str(value).map_err(|_| invalid())?;
        }
        _ if name.starts_with("data-") || name.starts_with("aria-") => {}
        _ => {
            return Err(format!(
            "SVG {name} is unsupported; simplify or explicitly rasterize the source before import"
        ))
        }
    }
    Ok(0)
}

struct Graph {
    children: Vec<usize>,
    refs: Vec<usize>,
    geometry: usize,
    growth: f64,
}
fn expansion(
    i: usize,
    graph: &[Graph],
    state: &mut [u8],
    sizes: &mut [(usize, usize, usize, f64)],
    depth: usize,
) -> Result<(usize, usize, usize, f64), String> {
    if depth > 64 || state[i] == 1 {
        return Err("SVG references are cyclic or too deeply nested".into());
    }
    if state[i] == 2 {
        if depth + sizes[i].2 > 64 {
            return Err("SVG expanded nesting limit exceeded".into());
        }
        return Ok(sizes[i]);
    }
    state[i] = 1;
    let mut size = (1usize, graph[i].geometry, 0usize, graph[i].growth);
    for next in graph[i].children.iter().chain(&graph[i].refs) {
        let n = expansion(*next, graph, state, sizes, depth + 1)?;
        size.0 = size
            .0
            .checked_add(n.0)
            .ok_or("SVG expansion limit exceeded")?;
        size.1 = size
            .1
            .checked_add(n.1)
            .ok_or("SVG expansion limit exceeded")?;
        size.2 = size.2.max(n.2 + 1);
        size.3 = size.3.max(graph[i].growth * n.3);
        if size.0 > NODES || size.1 > GEOMETRY {
            return Err("SVG reference expansion exceeds rendering limits".into());
        }
        if depth + size.2 > 64 || !size.3.is_finite() || size.3 > 100000. {
            return Err("SVG expanded transforms or nesting exceed finite bounds".into());
        }
    }
    state[i] = 2;
    sizes[i] = size;
    Ok(size)
}
pub fn tree(markup: &str) -> Result<usvg::Tree, String> {
    if markup.len() > MARKUP_LIMIT {
        return Err("SVG source exceeds 512 KiB".into());
    }
    if markup.contains("<!DOCTYPE") {
        return Err("SVG DTD/entity declarations are unsupported".into());
    }
    let xml = roxmltree::Document::parse_with_options(
        markup,
        roxmltree::ParsingOptions {
            allow_dtd: false,
            nodes_limit: (NODES * 4) as u32,
            entity_resolver: None,
        },
    )
    .map_err(|e| format!("Invalid SVG XML: {e}"))?;
    let root = xml.root_element();
    if root.tag_name().name() != "svg" || root.tag_name().namespace().is_some_and(|ns| ns != SVG_NS)
    {
        return Err("Expected an SVG document".into());
    }
    if root.attribute("viewBox").is_none()
        && (root.attribute("width").is_none() || root.attribute("height").is_none())
    {
        return Err("SVG needs an explicit viewport size or viewBox".into());
    }
    if root.attribute("viewBox").is_none()
        && ["width", "height"].iter().any(|name| {
            root.attribute(*name).is_some_and(|value| {
                svgtypes::Length::from_str(value)
                    .is_ok_and(|length| length.unit == svgtypes::LengthUnit::Percent)
            })
        })
    {
        return Err("SVG percentage viewport dimensions require an explicit viewBox".into());
    }
    if xml.descendants().any(|n| n.is_pi()) {
        return Err("SVG processing instructions/external stylesheets are unsupported".into());
    }
    let nodes: Vec<_> = root.descendants().filter(|n| n.is_element()).collect();
    if nodes.len() > NODES {
        return Err("SVG node limit exceeded".into());
    }
    let index: HashMap<_, _> = nodes.iter().enumerate().map(|(i, n)| (n.id(), i)).collect();
    let mut ids = HashMap::new();
    for (i, node) in nodes.iter().enumerate() {
        if let Some(id) = node.attribute("id") {
            if id.is_empty() || ids.insert(id, i).is_some() {
                return Err("SVG element IDs must be unique and nonempty".into());
            }
        }
    }
    let mut graph = Vec::with_capacity(nodes.len());
    for node in &nodes {
        if node.ancestors().count() > 66 {
            return Err("SVG nesting limit exceeded".into());
        }
        let metadata = node.ancestors().any(|n| {
            n.is_element()
                && ["metadata", "title", "desc"].contains(&n.tag_name().name())
                && n.tag_name().namespace().is_none_or(|ns| ns == SVG_NS)
        });
        let mut references = Vec::new();
        let mut geometry = 0usize;
        let mut growth = 1f64;
        let tag = node.tag_name().name();
        if !metadata {
            if node.tag_name().namespace().is_some_and(|ns| ns != SVG_NS)
                || ![
                    "svg",
                    "g",
                    "defs",
                    "symbol",
                    "use",
                    "path",
                    "rect",
                    "circle",
                    "ellipse",
                    "line",
                    "polyline",
                    "polygon",
                    "linearGradient",
                    "radialGradient",
                    "stop",
                    "clipPath",
                ]
                .contains(&tag)
            {
                return Err(format!("SVG {tag} content is unsupported; outline text and remove images, filters or other unsupported visuals before import"));
            }
            if node
                .children()
                .any(|n| n.is_text() && !n.text().unwrap_or("").trim().is_empty())
            {
                return Err("SVG visual text must be converted to paths before importing".into());
            }
            if ["rect", "circle", "ellipse", "line"].contains(&tag) {
                geometry = 4;
            }
            for attr in node.attributes() {
                if let Some(ns) = attr.namespace() {
                    if ns == "http://www.w3.org/1999/xlink" && attr.name() == "href" {
                    } else if ns == "http://www.w3.org/XML/1998/namespace" {
                        return Err("SVG xml:base/space controls are unsupported".into());
                    } else {
                        continue;
                    } // Retain nonvisual editor metadata in the original source.
                }
                if attr.name() == "style" {
                    for declaration in attr.value().split(';').filter(|s| !s.trim().is_empty()) {
                        let (name, value) = declaration
                            .split_once(':')
                            .ok_or("Invalid SVG inline style")?;
                        if ![
                            "fill",
                            "stroke",
                            "color",
                            "stop-color",
                            "opacity",
                            "fill-opacity",
                            "stroke-opacity",
                            "stop-opacity",
                            "clip-path",
                            "stroke-width",
                            "stroke-dashoffset",
                            "stroke-miterlimit",
                            "stroke-dasharray",
                            "fill-rule",
                            "clip-rule",
                            "stroke-linecap",
                            "stroke-linejoin",
                            "display",
                            "visibility",
                            "overflow",
                            "shape-rendering",
                            "color-interpolation",
                            "vector-effect",
                            "paint-order",
                        ]
                        .contains(&name.trim())
                        {
                            return Err(format!("SVG inline style {} is unsupported", name.trim()));
                        }
                        geometry += property(name.trim(), value, &mut references)?;
                    }
                } else {
                    geometry += property(attr.name(), attr.value(), &mut references)?;
                }
                if ["transform", "gradientTransform"].contains(&attr.name()) {
                    let t = svgtypes::Transform::from_str(attr.value())
                        .map_err(|_| "Invalid SVG transform")?;
                    growth *= (t.a.abs() + t.c.abs() + t.e.abs())
                        .max(t.b.abs() + t.d.abs() + t.f.abs())
                        .max(1.);
                }
            }
            if tag == "use" && references.is_empty() {
                return Err("SVG use needs a local source reference".into());
            }
        }
        let refs = references
            .into_iter()
            .map(|(id, kind)| {
                let target = *ids
                    .get(id)
                    .ok_or_else(|| format!("SVG references missing local element {id}"))?;
                if nodes[target].ancestors().any(|n| {
                    n.is_element()
                        && ["metadata", "title", "desc"].contains(&n.tag_name().name())
                        && n.tag_name().namespace().is_none_or(|ns| ns == SVG_NS)
                }) {
                    return Err("SVG references cannot turn nonvisual metadata into artwork".into());
                }
                let target_tag = nodes[target].tag_name().name();
                let valid = match kind {
                    "paint" => ["linearGradient", "radialGradient"].contains(&target_tag),
                    "clip" => target_tag == "clipPath",
                    "href" if tag == "use" => [
                        "svg", "g", "symbol", "path", "rect", "circle", "ellipse", "line",
                        "polyline", "polygon", "use",
                    ]
                    .contains(&target_tag),
                    "href" if ["linearGradient", "radialGradient"].contains(&tag) => {
                        ["linearGradient", "radialGradient"].contains(&target_tag)
                    }
                    _ => false,
                };
                if !valid {
                    return Err("SVG reference target is unsupported for this property".into());
                }
                Ok(target)
            })
            .collect::<Result<Vec<_>, String>>()?;
        graph.push(Graph {
            children: node
                .children()
                .filter_map(|n| index.get(&n.id()).copied())
                .collect(),
            refs,
            geometry,
            growth,
        });
    }
    let expanded = expansion(
        0,
        &graph,
        &mut vec![0; graph.len()],
        &mut vec![(0, 0, 0, 1.); graph.len()],
        0,
    )?;
    let mut options = usvg::Options::default();
    options.resources_dir = None;
    options.image_href_resolver = usvg::ImageHrefResolver {
        resolve_data: Box::new(|_, _, _| None),
        resolve_string: Box::new(|_, _| None),
    };
    let tree =
        usvg::Tree::from_xmltree(&xml, &options).map_err(|e| format!("Cannot render SVG: {e}"))?;
    let w = tree.size().width().ceil() as u32;
    let h = tree.size().height().ceil() as u32;
    check_size(w, h)?;
    if u64::from(w) * u64::from(h) > 4_000_000 {
        return Err("SVG source frames are limited to four million pixels".into());
    }
    if u64::from(w) * u64::from(h) * (expanded.1.div_ceil(32).max(1) as u64) > 256_000_000 {
        return Err("SVG source geometry exceeds bounded rendering work".into());
    }
    Ok(tree)
}
pub fn dimensions(markup: &str) -> Result<(u32, u32), String> {
    let tree = tree(markup)?;
    Ok((
        tree.size().width().ceil() as u32,
        tree.size().height().ceil() as u32,
    ))
}
fn complexity(
    group: &usvg::Group,
    depth: usize,
    paths: &mut usize,
    segments: &mut usize,
    maximum: &mut usize,
) -> Result<(), String> {
    *maximum = (*maximum).max(depth);
    if depth > 32 {
        return Err("SVG rendered group nesting limit exceeded".into());
    }
    for node in group.children() {
        match node {
            usvg::Node::Group(g) => complexity(g, depth + 1, paths, segments, maximum)?,
            usvg::Node::Path(p) => {
                *paths += 1;
                *segments += p.data().segments().count();
                if *paths > NODES || *segments > GEOMETRY {
                    return Err("SVG rendered geometry limit exceeded".into());
                }
            }
            _ => return Err("SVG renderer contains an unsupported image or text node".into()),
        }
    }
    Ok(())
}
/// Isolated groups can allocate up to the renderer's five-canvas bounding box
/// on each axis. Account for actual transformed boxes and conservative nested
/// clip buffers before allowing the renderer to allocate any output surface.
fn surfaces(
    group: &usvg::Group,
    transform: tiny_skia::Transform,
    live: u64,
    clip_buffers: u64,
    canvas: [u32; 2],
    maximum: &mut u64,
    largest: &mut u64,
) -> Result<(), String> {
    for node in group.children() {
        if let usvg::Node::Group(child) = node {
            let transform = transform.pre_concat(child.transform());
            let area = if child.should_isolate() {
                child
                    .layer_bounding_box()
                    .transform(transform)
                    .map_or(0, |bounds| {
                        let w = (bounds.width().ceil() as u64)
                            .saturating_add(4)
                            .min(u64::from(canvas[0]) * 5);
                        let h = (bounds.height().ceil() as u64)
                            .saturating_add(4)
                            .min(u64::from(canvas[1]) * 5);
                        w.saturating_mul(h)
                    })
            } else {
                0
            };
            *largest = (*largest).max(area);
            let bytes = area.saturating_mul(4);
            let own = live.saturating_add(bytes);
            let peak = own.saturating_add(if child.clip_path().is_some() {
                bytes.saturating_mul(clip_buffers)
            } else {
                0
            });
            *maximum = (*maximum).max(peak);
            if *maximum > 128 * 1024 * 1024 {
                return Err("SVG projection exceeds bounded rendering memory".into());
            }
            surfaces(
                child,
                transform,
                own,
                clip_buffers,
                canvas,
                maximum,
                largest,
            )?;
        }
    }
    Ok(())
}
pub fn render(
    markup: &str,
    intrinsic: [u32; 2],
    matrix: [f32; 6],
    w: u32,
    h: u32,
    depth: u16,
) -> Result<Raster, String> {
    let tree = tree(markup)?;
    if intrinsic
        != [
            tree.size().width().ceil() as u32,
            tree.size().height().ceil() as u32,
        ]
    {
        return Err("SVG viewport must match its editable source frame".into());
    }
    check_size(w, h)?;
    let (mut paths, mut segments, mut nesting) = (0, 0, 0);
    complexity(tree.root(), 0, &mut paths, &mut segments, &mut nesting)?;
    for clip in tree.clip_paths() {
        complexity(clip.root(), 0, &mut paths, &mut segments, &mut nesting)?;
    }
    let area = u64::from(w) * u64::from(h);
    let [a, b, c, d, x, y] = matrix;
    let transform = tiny_skia::Transform::from_scale(
        intrinsic[0] as f32 / tree.size().width(),
        intrinsic[1] as f32 / tree.size().height(),
    )
    .post_concat(tiny_skia::Transform::from_row(a, b, c, d, x, y));
    let (mut memory, mut largest) = (0, area);
    let clip_buffers = ((nesting + 2) as u64).saturating_mul((tree.clip_paths().len() + 1) as u64);
    surfaces(
        tree.root(),
        transform,
        0,
        clip_buffers,
        [w, h],
        &mut memory,
        &mut largest,
    )?;
    if largest.saturating_mul(paths.max(segments.div_ceil(32)).max(1) as u64) > 256_000_000
        || area
            .saturating_mul(20)
            .saturating_add(memory)
            .saturating_add(1024 * 1024)
            > 128 * 1024 * 1024
    {
        return Err("SVG projection exceeds bounded rendering work or memory".into());
    }
    let mut pixmap = tiny_skia::Pixmap::new(w, h).ok_or("Cannot allocate SVG projection")?;
    resvg::render(&tree, transform, &mut pixmap.as_mut());
    let mut bytes = Vec::with_capacity((area * 4) as usize);
    for pixel in pixmap.pixels() {
        let p = pixel.demultiply();
        bytes.extend_from_slice(&[p.red(), p.green(), p.blue(), p.alpha()]);
    }
    let mut raster = Raster::from_rgba(w, h, &bytes)?;
    if depth == 16 {
        raster.promote16();
    } else if depth != 8 {
        return Err("SVG projection requires native8/16 document channels".into());
    }
    Ok(raster)
}
