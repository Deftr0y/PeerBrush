//! Source-independent command scope policy, matching the shared Engine.
use super::*;
use crate::engine::{num, rect, text, Scope};
use serde_json::Value;
fn visibility_only(c: &Value) -> bool {
    c["op"] == "layer.update"
        && c["visible"].is_boolean()
        && c.as_object().is_some_and(|object| {
            object
                .keys()
                .all(|key| ["op", "layer", "visible"].contains(&key.as_str()))
        })
}
fn polygon(metadata: &Document36) -> Option<&[[f32; 2]]> {
    let rectangle = metadata.selection?;
    let polygon = metadata.selection_polygon.as_deref()?;
    (crate::selection::bounds(polygon).ok() == Some(rectangle)).then_some(polygon)
}
fn warp_bounds(command: &Value, bounds: [f64; 4]) -> Result<[f64; 4]> {
    let grid = crate::warp::parse(command)?;
    let warp = grid.as_ref().map(crate::warp::Grid::prepare).transpose()?;
    let projective = crate::projective::parse(command)?;
    let pivot = projective
        .map(|p| p.center())
        .or_else(|| {
            grid.as_ref()
                .map(|g| [(g.rect[0] + g.rect[2]) * 0.5, (g.rect[1] + g.rect[3]) * 0.5])
        })
        .unwrap_or([0., 0.]);
    let after = crate::projective::affine(command, pivot)?
        .compose(projective.map_or(crate::projective::Map::IDENTITY, |p| p.map))?;
    after.bounds(if let Some(warp) = warp {
        warp.bounds(bounds)?
    } else {
        bounds
    })
}
fn roots(doc: &Document36, selected: &[String]) -> Vec<String> {
    doc.layers
        .iter()
        .filter(|layer| {
            selected.contains(&layer.id) && {
                let mut parent = layer.parent.as_deref();
                let mut contained = false;
                for _ in 0..=16 {
                    let Some(id) = parent else { break };
                    if selected.iter().any(|selected| selected == id) {
                        contained = true;
                        break;
                    }
                    parent = doc
                        .layers
                        .iter()
                        .find(|layer| layer.id == id)
                        .and_then(|layer| layer.parent.as_deref());
                }
                !contained
            }
        })
        .map(|layer| layer.id.clone())
        .collect()
}
pub(super) fn scopes(metadata: &Document36, c: &Value) -> Vec<Scope> {
    let mut scopes = command_scopes(metadata, c);
    if metadata.selection.is_some()
        && (c["op"] == "paint.clear_selection"
            || (matches!(c["op"].as_str(), Some("move" | "transform"))
                && c["selection_only"] != false))
    {
        scopes.push(Scope::layer("@selection"));
    }
    if c["op"] == "mask.from_selection" || (c["op"] == "mask.add" && metadata.selection.is_some()) {
        scopes.push(Scope::layer("@selection"));
    }
    scopes
}
fn command_scopes(metadata: &Document36, c: &Value) -> Vec<Scope> {
    let op = text(c, "op", "");
    if op.starts_with("filter.") {
        return vec![Scope {
            target: None,
            rect: None,
        }];
    }
    let target = c.get("layer").and_then(Value::as_str).map(String::from);
    if matches!(op, "path.to_selection" | "path.to_mask") {
        let mut scopes = vec![Scope::layer(c["path"].as_str().unwrap_or("@invalid_path"))];
        if op == "path.to_selection" {
            scopes.push(Scope::layer("@selection"));
        } else {
            scopes.push(Scope { target, rect: None });
        }
        return scopes;
    }
    if matches!(op, "effect.bake" | "mask.step.bake") {
        return vec![Scope { target, rect: None }];
    }
    if visibility_only(c) {
        return vec![Scope {
            target: target.map(|t| format!("@visibility:{t}")),
            rect: None,
        }];
    }
    if op == "selection" || op.starts_with("selection.") {
        return vec![Scope {
            target: Some("@selection".into()),
            rect: None,
        }];
    }
    if ["move", "transform"].contains(&op)
        && target.as_ref().is_some_and(|t| {
            metadata
                .layers
                .iter()
                .any(|l| l.id == *t && l.kind == "group")
        })
    {
        return vec![Scope { target, rect: None }];
    }
    if op == "layer.merge" {
        return requested(c)
            .and_then(|ids| resolve(metadata, &ids))
            .map(|ids| ids.into_iter().map(|id| Scope::layer(&id)).collect())
            .unwrap_or_else(|_| {
                vec![Scope {
                    target: None,
                    rect: None,
                }]
            });
    }
    if ["layer.add", "image.import", "source.add", "path.create"].contains(&op) {
        return c["parent"]
            .as_str()
            .map(|p| {
                vec![Scope {
                    target: Some(p.into()),
                    rect: None,
                }]
            })
            .unwrap_or_default();
    }
    if [
        "layer.reorder",
        "layer.delete",
        "layer.merge",
        "layer.duplicate",
        "layer.duplicate_selection",
        "layer.paste",
        "group.create_selected",
        "adjustment.add",
        "layer.clip",
        "layer.parent",
        "image.paste",
        "image.place",
    ]
    .contains(&op)
        && !(op == "image.place" && c["new_layer"] == false)
    {
        let mut targets = vec![];
        if let Some(t) = target.clone() {
            targets.push(t);
        }
        if let Some(ids) = c["layers"].as_array() {
            targets.extend(ids.iter().filter_map(Value::as_str).map(str::to_owned));
        }
        for key in ["parent", "clip_to", "base"] {
            if let Some(t) = c[key].as_str() {
                targets.push(t.into());
            }
        }
        // Clip changes can alter their old base's composited unit.
        if op == "layer.clip" {
            if let Some(t) = target
                .as_ref()
                .and_then(|t| metadata.layers.iter().find(|l| l.id == *t))
                .and_then(|l| l.clip_to.clone())
            {
                targets.push(t);
            }
        }
        if op == "adjustment.add" && targets.is_empty() {
            return vec![Scope {
                target: None,
                rect: None,
            }];
        }
        targets.sort();
        targets.dedup();
        return targets
            .into_iter()
            .map(|t| Scope {
                target: Some(t),
                rect: None,
            })
            .collect();
    }
    if matches!(op, "paint.fill" | "paint.clear_selection") {
        let layer = target
            .as_deref()
            .and_then(|id| metadata.layers.iter().find(|l| l.id == id));
        if layer.is_some_and(|l| l.kind == "group") && c["mask"] != true {
            return vec![Scope { target, rect: None }];
        }
        let mut area = if op == "paint.clear_selection" {
            metadata.selection
        } else {
            c.get("rect").and_then(rect).or(metadata.selection)
        }
        .unwrap_or([0, 0, metadata.width as i32, metadata.height as i32]);
        if let Some(selection) = metadata.selection {
            area = [
                area[0].max(selection[0]),
                area[1].max(selection[1]),
                area[2].min(selection[2]),
                area[3].min(selection[3]),
            ];
        }
        return vec![Scope {
            target,
            rect: Some(area),
        }];
    }
    if op == "image.place" && c["new_layer"] == false {
        return vec![Scope {
            target,
            rect: c.get("rect").and_then(rect),
        }];
    }
    if [
        "new",
        "crop",
        "resize",
        "canvas.resize",
        "image.resize",
        "document.settings",
        "layer.add",
        "layer.reorder",
        "layer.delete",
        "layer.merge",
        "layer.duplicate",
        "layer.duplicate_selection",
        "layer.paste",
        "group.create_selected",
        "adjustment.add",
        "layer.clip",
        "layer.parent",
        "image.import",
        "image.paste",
        "image.place",
    ]
    .contains(&op)
    {
        return vec![Scope {
            target: None,
            rect: None,
        }];
    }
    let mut area = if ["paint", "smudge", "clone", "heal", "liquify.stroke"].contains(&op) {
        c.get("points").and_then(Value::as_array).map(|p| {
            let r = num(c, "radius", 10.0) as f32 + 2.0;
            let mut a = [f32::MAX, f32::MAX, f32::MIN, f32::MIN];
            for point in p {
                if let Some(pair) = point.as_array() {
                    let x = pair.first().and_then(Value::as_f64).unwrap_or(0.0) as f32;
                    let y = pair.get(1).and_then(Value::as_f64).unwrap_or(0.0) as f32;
                    a = [
                        a[0].min(x - r),
                        a[1].min(y - r),
                        a[2].max(x + r),
                        a[3].max(y + r),
                    ];
                }
            }
            [
                a[0].floor() as i32,
                a[1].floor() as i32,
                a[2].ceil() as i32,
                a[3].ceil() as i32,
            ]
        })
    } else {
        None
    };
    if ["shape", "gradient", "fill", "adjust", "pixels.replace"].contains(&op) {
        area = c["rect"]
            .as_array()
            .and_then(|_| rect(&c["rect"]))
            .or(metadata.selection);
    }
    if op == "transform" && c.get("warp").is_some() {
        match crate::warp::parse(c) {
            Ok(Some(_)) => {
                let area = metadata
                    .selection
                    .filter(|_| c["selection_only"] != false)
                    .and_then(|b| {
                        warp_bounds(c, b.map(|v| v as f64)).ok().map(|p| {
                            [
                                b[0].min(p[0].floor() as i32),
                                b[1].min(p[1].floor() as i32),
                                b[2].max(p[2].ceil() as i32),
                                b[3].max(p[3].ceil() as i32),
                            ]
                        })
                    });
                return vec![Scope { target, rect: area }];
            }
            Err(_) => return vec![Scope { target, rect: None }],
            Ok(None) => {}
        }
    }
    if op == "transform" && c.get("projective").is_some() {
        match crate::projective::combined(c) {
            Ok(Some(map)) => {
                let area = metadata
                    .selection
                    .filter(|_| c["selection_only"] != false)
                    .and_then(|b| {
                        map.bounds(b.map(|v| v as f64)).ok().map(|p| {
                            [
                                b[0].min(p[0].floor() as i32),
                                b[1].min(p[1].floor() as i32),
                                b[2].max(p[2].ceil() as i32),
                                b[3].max(p[3].ceil() as i32),
                            ]
                        })
                    });
                return vec![Scope { target, rect: area }];
            }
            Err(_) => return vec![Scope { target, rect: None }],
            Ok(None) => {}
        }
    }
    if ["move", "transform"].contains(&op) && c["selection_only"] != false {
        if let Some(b) = metadata.selection {
            let moving = op == "move";
            let [dx, dy] = if moving {
                [num(c, "dx", 0.).round(), num(c, "dy", 0.).round()]
            } else {
                crate::transform::translation(c).unwrap_or([0., 0.])
            };
            let angle = if moving {
                0.
            } else {
                num(c, "angle", 0.).to_radians()
            };
            let (sin, cos) = angle.sin_cos();
            let (sx, sy) = if moving {
                (1., 1.)
            } else {
                crate::transform::scales(c).unwrap_or((1., 1.))
            };
            let px = c["pivot"][0]
                .as_f64()
                .unwrap_or((b[0] as f64 + b[2] as f64) / 2.);
            let py = c["pivot"][1]
                .as_f64()
                .unwrap_or((b[1] as f64 + b[3] as f64) / 2.);
            let shear = crate::transform::selected_shear(c).unwrap_or([0., 0.]);
            let linear = [cos * sx, sin * sx, -sin * sy, cos * sy];
            let skew = crate::transform::skewed(linear, c)
                .ok()
                .filter(|_| shear != [0., 0.]);
            let mut out = b;
            for p in crate::selection::rectangle(b) {
                let (x, y) = if let Some([a, bb, cc, d]) = skew {
                    (
                        px + a * (p[0] as f64 - px) + cc * (p[1] as f64 - py) + dx,
                        py + bb * (p[0] as f64 - px) + d * (p[1] as f64 - py) + dy,
                    )
                } else {
                    (
                        px + (p[0] as f64 - px) * sx * cos - (p[1] as f64 - py) * sy * sin + dx,
                        py + (p[0] as f64 - px) * sx * sin + (p[1] as f64 - py) * sy * cos + dy,
                    )
                };
                out = [
                    out[0].min(x.floor() as i32),
                    out[1].min(y.floor() as i32),
                    out[2].max(x.ceil() as i32),
                    out[3].max(y.ceil() as i32),
                ];
            }
            if shear != [0., 0.] || c["fractional_translation"] == true {
                let (mut sin, mut cos) = (num(c, "angle", 0.) as f32).to_radians().sin_cos();
                if sin.abs() < 0.000001 {
                    sin = 0.;
                }
                if cos.abs() < 0.000001 {
                    cos = 0.;
                }
                let rectangle = crate::selection::rectangle(b);
                let covered = metadata
                    .selection_coverage
                    .as_ref()
                    .is_some_and(|coverage| coverage.bounds == b);
                let points = if covered {
                    rectangle.as_slice()
                } else {
                    polygon(metadata).unwrap_or(&rectangle)
                };
                for &p in points {
                    let [x, y] = crate::transform::selected_point(
                        p,
                        [px as f32, py as f32],
                        [dx as f32, dy as f32],
                        [sx as f32, sy as f32],
                        [sin, cos],
                        shear,
                    );
                    out = [
                        out[0].min(x.floor() as i32),
                        out[1].min(y.floor() as i32),
                        out[2].max(x.ceil() as i32),
                        out[3].max(y.ceil() as i32),
                    ];
                }
            }
            area = Some(out);
        }
    }
    let area = area.map(|a| {
        if ["move", "transform"].contains(&op) {
            return a;
        }
        metadata.selection.map_or(a, |b| {
            [
                a[0].max(b[0]),
                a[1].max(b[1]),
                a[2].min(b[2]),
                a[3].min(b[3]),
            ]
        })
    });
    vec![Scope { target, rect: area }]
}
fn requested(command: &Value) -> Result<Vec<String>> {
    if let Some(ids) = command.get("layers") {
        let ids = ids
            .as_array()
            .ok_or("Merge layers must be an array of layer IDs")?;
        ids.iter()
            .map(|v| {
                v.as_str()
                    .filter(|s| !s.is_empty())
                    .map(String::from)
                    .ok_or_else(|| "Merge needs valid layer IDs".into())
            })
            .collect()
    } else {
        Ok(vec![command["layer"]
            .as_str()
            .filter(|s| !s.is_empty())
            .ok_or("Choose layers to merge")?
            .into()])
    }
}

fn resolve(doc: &Document36, ids: &[String]) -> Result<Vec<String>> {
    if ids.is_empty() || ids.len() > 100 {
        return Err("Choose 1–100 layers to merge".into());
    }
    for id in ids {
        if !doc.layers.iter().any(|l| l.id == *id) {
            return Err("A selected layer no longer exists".into());
        }
    }
    let mut roots = roots(doc, ids);
    if roots.len() == 1 {
        let at = doc.layers.iter().position(|l| l.id == roots[0]).unwrap();
        let selected = &doc.layers[at];
        if selected.kind != "group" {
            let below = doc
                .layers
                .iter()
                .skip(at + 1)
                .find(|l| l.parent == selected.parent)
                .ok_or("There is no layer below this one to merge")?;
            roots.push(below.id.clone());
        }
    }
    let parent = &doc.layers.iter().find(|l| l.id == roots[0]).unwrap().parent;
    if roots
        .iter()
        .any(|id| doc.layers.iter().find(|l| l.id == *id).unwrap().parent != *parent)
    {
        return Err("Choose layers in the same folder to merge".into());
    }
    if roots
        .iter()
        .any(|id| !doc.layers.iter().find(|l| l.id == *id).unwrap().visible)
    {
        return Err("Show the selected layers before merging".into());
    }
    if roots.len() > 1 {
        let siblings: Vec<_> = doc.layers.iter().filter(|l| l.parent == *parent).collect();
        let positions: Vec<_> = siblings
            .iter()
            .enumerate()
            .filter_map(|(i, l)| roots.contains(&l.id).then_some(i))
            .collect();
        if positions.last().unwrap() - positions[0] + 1 != roots.len() {
            return Err("Choose adjacent layers to preserve the canvas when merging".into());
        }
        if siblings
            .iter()
            .filter(|l| roots.contains(&l.id))
            .any(|l| l.blend != "normal")
        {
            return Err(
                "Put these layers in a folder and merge the folder to preserve their blend modes"
                    .into(),
            );
        }
    }
    Ok(roots)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::scene::RasterCollector;
    use serde_json::json;
    struct ZeroSources {
        sources: Vec<Surface>,
    }
    impl RasterCollector for ZeroSources {
        fn resident(&mut self, raster: &crate::raster::Raster) -> Result<RasterRef36> {
            if !raster.tiles.is_empty() || !raster.samples16.is_empty() || raster.retained.is_some()
            {
                return Err("Scope fixture must contain actual known-zero planes".into());
            }
            self.sources.push(
                Surface::zero(raster.width, raster.height, raster.depth)
                    .map_err(|e| e.to_string())?,
            );
            Ok(RasterRef36 {
                id: self.sources.len() as u32,
                width: raster.width,
                height: raster.height,
                depth: raster.depth,
                retained: None,
            })
        }
        fn surface(&mut self, _: &Surface) -> Result<RasterRef36> {
            Err("No native source conversion in scope fixture".into())
        }
    }
    #[test]
    fn scope_policy_matches_actual_legacy_engine_for_native8_and16_geometry_and_selections() {
        for depth in [8, 16] {
            let mut legacy = crate::engine::Engine::new();
            legacy.doc = crate::engine::Document::new_depth(512, 256, depth).unwrap();
            legacy.doc.layers[0].id = "paint".into();
            let mut group = crate::engine::Layer::new("Folder", "group", 512, 256);
            group.id = "group".into();
            group.pixels = crate::raster::Raster::new_depth(512, 256, depth);
            legacy.doc.layers.push(group);
            let mut child = crate::engine::Layer::new("Child", "paint", 512, 256);
            child.id = "child".into();
            child.parent = Some("group".into());
            child.pixels = crate::raster::Raster::new_depth(512, 256, depth);
            legacy.doc.layers.push(child);
            for selected in [false, true] {
                legacy.doc.selection = selected.then_some([1, 2, 20, 22]);
                legacy.doc.selection_polygon =
                    selected.then_some(vec![[1., 2.], [20., 2.], [1., 22.]]);
                let mut source = ZeroSources { sources: vec![] };
                let metadata = Document36::capture(&legacy.doc, &mut source).unwrap();
                let grid = json!({"rect":[0.,0.,24.,24.],"points":[[0.,0.],[12.,0.],[24.,0.],[0.,12.],[12.,13.],[24.,12.],[0.,24.],[12.,24.],[24.,24.]]});
                let commands = vec![
                    json!({"op":"layer.update","layer":"paint","visible":false}),
                    json!({"op":"layer.update","layer":"paint","name":"Renamed"}),
                    json!({"op":"paint","layer":"paint","points":[[255.25,3.],[258.5,8.]],"radius":2.25}),
                    json!({"op":"paint.fill","layer":"paint","rect":[0,0,12,16]}),
                    json!({"op":"paint.clear_selection","layer":"paint"}),
                    json!({"op":"paint.fill","layer":"group"}),
                    json!({"op":"shape","layer":"paint","rect":[0,0,12,16]}),
                    json!({"op":"pixels.replace","layer":"paint","rect":[0,0,256,256]}),
                    json!({"op":"move","layer":"paint","dx":3.2,"dy":-4.5}),
                    json!({"op":"transform","layer":"paint","angle":12.5,"scale_x":1.2,"scale_y":0.8}),
                    json!({"op":"transform","layer":"paint","warp":grid}),
                    json!({"op":"transform","layer":"paint","warp":{"bad":true}}),
                    json!({"op":"move","layer":"group","dx":4}),
                    json!({"op":"transform","layer":"paint","selection_only":false}),
                    json!({"op":"mask.add","layer":"paint"}),
                    json!({"op":"mask.from_selection","layer":"paint"}),
                    json!({"op":"selection","kind":"wand"}),
                    json!({"op":"selection.clear"}),
                    json!({"op":"filter.update","id":"filter"}),
                    json!({"op":"layer.add","parent":"group"}),
                    json!({"op":"layer.reorder","layer":"child","parent":"group","index":0}),
                    json!({"op":"layer.merge","layers":["paint"]}),
                    json!({"op":"layer.merge","layers":["group"]}),
                    json!({"op":"image.place","layer":"paint","new_layer":false,"rect":[0,0,7,9]}),
                    json!({"op":"image.import"}),
                    json!({"op":"canvas.resize"}),
                ];
                for command in commands {
                    assert_eq!(
                        scopes(&metadata, &command),
                        legacy.scopes(&command),
                        "{depth} {selected} {command}"
                    );
                }
            }
        }
    }
}
