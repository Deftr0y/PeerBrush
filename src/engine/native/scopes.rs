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
        && matches!(c["op"].as_str(), Some("move" | "transform"))
        && c["selection_only"] != false
    {
        if let Some(layer) = c["layer"]
            .as_str()
            .and_then(|id| metadata.layers.iter().find(|layer| layer.id == id))
        {
            if let Some(mask) = &layer.mask {
                for scope in &mut scopes {
                    if let Some(area) = &mut scope.rect {
                        *area = crate::engine::reframe_scope::widen(
                            *area,
                            [layer.x, layer.y],
                            [layer.pixels.width, layer.pixels.height],
                            mask.linked,
                            mask.steps
                                .iter()
                                .map(|step| [step.pixels.width, step.pixels.height]),
                        );
                    }
                }
            }
        }
    }
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
    if op.starts_with("filter.")
        || crate::engine::document_settings::may_promote(metadata.bit_depth, c)
        || crate::engine::document_settings::may_initialize_global_light(metadata.psd_global_angle, c)
    {
        return vec![Scope {
            target: None,
            rect: None,
        }];
    }
    if matches!(op, "image.paste" | "layer.paste") {
        let layer = |i: usize| {
            let l = &metadata.layers[i];
            crate::clipboard::paste::Layer {
                id: &l.id,
                parent: l.parent.as_deref(),
                kind: &l.kind,
                clip_to: l.clip_to.as_deref(),
                locked: l.locked,
            }
        };
        let plan = if op == "layer.paste" {
            crate::clipboard::paste::Plan::layers(
                text(c, "layer", ""),
                metadata.layers.len(),
                1,
                layer,
            )
        } else {
            crate::clipboard::paste::Plan::new(c, metadata.layers.len(), layer)
        };
        return plan
            .map(|plan| plan.scopes(metadata.layers.len(), layer))
            .unwrap_or_else(|_| {
                vec![Scope {
                    target: None,
                    rect: None,
                }]
            });
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
    if op == "fill"
        && c["mask"] != true
        && target.as_deref().is_some_and(|id| {
            metadata.layers.iter().any(|layer| {
                layer.id == id
                    && (layer.kind == "group"
                        || (layer.kind == "fill" && metadata.selection.is_none()))
            })
        })
    {
        return vec![Scope { target, rect: None }];
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
        return vec![Scope { target, rect: None }];
    }
    if [
        "new",
        "crop",
        "resize",
        "canvas.resize",
        "image.resize",
        "document.settings",
        "document.global_light",
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
            let r = if op == "liquify.stroke" {
                crate::liquify::command::radius(c)
            } else {
                num(c, "radius", 10.0)
            } as f32
                + 2.0;
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
    crate::merge::requested(command)
}
fn resolve(doc: &Document36, ids: &[String]) -> Result<Vec<String>> {
    crate::merge::input::resolve(doc.layers.len(), |i| (&doc.layers[i]).into(), ids).map(|roots| {
        roots
            .into_iter()
            .map(|i| doc.layers[i].id.clone())
            .collect()
    })
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
    fn selected_linked_mask_reframe_reserves_current_and_revealed_pixels() {
        for depth in [8, 16] {
            let mut legacy = crate::engine::Engine::new();
            legacy.resident_state_mut().expect("resident fixture").document = crate::engine::Document::new_depth(64, 64, depth).unwrap();
            legacy.resident_state_mut().expect("resident fixture").document.selection = Some([4, 6, 9, 12]);
            let layer = &mut legacy.resident_state_mut().expect("resident fixture").document.layers[0];
            layer.x = 2;
            layer.y = 3;
            layer.pixels = crate::raster::Raster::new_depth(17, 19, depth);
            let command = json!({"op":"transform","layer":layer.id,"angle":0});
            let mut source = ZeroSources { sources: vec![] };
            let mut metadata = Document36::capture(&legacy.resident_state().expect("resident fixture").document, &mut source).unwrap();
            metadata.layers[0].mask = Some(crate::engine::scene::Mask36 {
                linked: true,
                enabled: false,
                steps: [[7, 9], [31, 29]]
                    .into_iter()
                    .enumerate()
                    .map(
                        |(index, [width, height])| crate::engine::scene::MaskStep36 {
                            id: format!("step{index}"),
                            kind: if index == 0 { "fill" } else { "paint" }.into(),
                            enabled: false,
                            weight: 1.,
                            value: 1.,
                            settings: Value::Null.into(),
                            pixels: RasterRef36 {
                                id: index as u32 + 2,
                                width,
                                height,
                                depth,
                                retained: None,
                            },
                        },
                    )
                    .collect(),
            });
            let planned = scopes(&metadata, &command);
            assert_eq!(planned[0].rect, Some([2, 3, 33, 32]));
            // Collective reframe can reveal retained sample (16,18) at this
            // document pixel despite an identity transform of a smaller selection.
            let actual = [18, 21, 19, 22];
            let reserved = planned[0].rect.unwrap();
            assert!(
                reserved[0] <= actual[0]
                    && reserved[1] <= actual[1]
                    && reserved[2] >= actual[2]
                    && reserved[3] >= actual[3]
            );
            let mask = metadata.layers[0].mask.as_mut().unwrap();
            for step in &mut mask.steps {
                step.pixels.width = 17;
                step.pixels.height = 19;
            }
            assert_eq!(scopes(&metadata, &command)[0].rect, Some([4, 6, 9, 12]));
            metadata.layers[0].kind = "group".into();
            assert_eq!(scopes(&metadata, &command)[0].rect, None);
        }
    }
    #[test]
    fn scope_policy_matches_actual_legacy_engine_for_native8_and16_geometry_and_selections() {
        for depth in [8, 16] {
            let mut legacy = crate::engine::Engine::new();
            legacy.resident_state_mut().expect("resident fixture").document = crate::engine::Document::new_depth(512, 256, depth).unwrap();
            legacy.resident_state_mut().expect("resident fixture").document.layers[0].id = "paint".into();
            let mut group = crate::engine::Layer::new("Folder", "group", 512, 256);
            group.id = "group".into();
            group.pixels = crate::raster::Raster::new_depth(512, 256, depth);
            legacy.resident_state_mut().expect("resident fixture").document.layers.push(group);
            let mut child = crate::engine::Layer::new("Child", "paint", 512, 256);
            child.id = "child".into();
            child.parent = Some("group".into());
            child.pixels = crate::raster::Raster::new_depth(512, 256, depth);
            legacy.resident_state_mut().expect("resident fixture").document.layers.push(child);
            for selected in [false, true] {
                legacy.resident_state_mut().expect("resident fixture").document.selection = selected.then_some([1, 2, 20, 22]);
                legacy.resident_state_mut().expect("resident fixture").document.selection_polygon =
                    selected.then_some(vec![[1., 2.], [20., 2.], [1., 22.]]);
                let mut source = ZeroSources { sources: vec![] };
                let metadata = Document36::capture(&legacy.resident_state().expect("resident fixture").document, &mut source).unwrap();
                let grid = json!({"rect":[0.,0.,24.,24.],"points":[[0.,0.],[12.,0.],[24.,0.],[0.,12.],[12.,13.],[24.,12.],[0.,24.],[12.,24.],[24.,24.]]});
                let commands = vec![
                    json!({"op":"layer.update","layer":"paint","visible":false}),
                    json!({"op":"layer.update","layer":"paint","name":"Renamed"}),
                    json!({"op":"paint","layer":"paint","points":[[255.25,3.],[258.5,8.]],"radius":2.25}),
                    json!({"op":"liquify.stroke","layer":"paint","points":[[100.,100.],[110.,105.]]}),
                    json!({"op":"liquify.stroke","layer":"group","points":[[100.,100.],[110.,105.]],"radius":3.25}),
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
                    json!({"op":"image.import","parent":"group"}),
                    json!({"op":"image.paste","layer":"child"}),
                    json!({"op":"layer.paste","parent":"group"}),
                    json!({"op":"effect.add","layer":"paint","kind":"pattern_overlay","settings":{"pattern":"bitmap","bitmap_png":"pending"}}),
                    json!({"op":"canvas.resize"}),
                ];
                for command in commands {
                    if command["op"] == "liquify.stroke"
                        && command.get("radius").is_none()
                        && !selected
                    {
                        assert_eq!(
                            scopes(&metadata, &command)[0].rect,
                            Some([58, 58, 152, 147])
                        );
                    }
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
