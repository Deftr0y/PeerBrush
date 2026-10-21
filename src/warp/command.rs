use super::{extrema, Grid, Mapping};
use crate::{
    engine::{Document, Layer},
    projective::{self, Map},
};
use serde_json::Value;

/// One current command. W first, then P, then the existing affine A.
pub(crate) struct CommandMap {
    pub(crate) grid: Option<Grid>,
    warp: Option<Mapping>,
    pub(crate) after: Map,
    inverse: Map,
}
impl CommandMap {
    pub(crate) fn new(command: &Value, default_pivot: [f64; 2]) -> Result<Self, String> {
        let grid = super::parse(command)?;
        let warp = grid.as_ref().map(Grid::prepare).transpose()?;
        let p = projective::parse(command)?;
        let pivot = p
            .map(|p| p.center())
            .or_else(|| {
                grid.as_ref()
                    .map(|g| [(g.rect[0] + g.rect[2]) * 0.5, (g.rect[1] + g.rect[3]) * 0.5])
            })
            .unwrap_or(default_pivot);
        let after =
            projective::affine(command, pivot)?.compose(p.map_or(Map::IDENTITY, |p| p.map))?;
        Ok(Self {
            grid,
            warp,
            after,
            inverse: after.inverse()?,
        })
    }
    pub(crate) fn point(&self, point: [f64; 2]) -> Result<[f64; 2], String> {
        self.after.point(if let Some(w) = &self.warp {
            w.point(point)?
        } else {
            point
        })
    }
    pub(crate) fn inverse(&self, point: [f64; 2]) -> Option<[f64; 2]> {
        let point = self.inverse.point(point).ok()?;
        if let Some(w) = &self.warp {
            w.inverse(point)
        } else {
            Some(point)
        }
    }
    pub(crate) fn sample(&self, point: [f64; 2], origin: [f64; 2]) -> [f32; 2] {
        self.inverse(point)
            .map(|p| [p[0] - origin[0] - 0.5, p[1] - origin[1] - 0.5])
            .filter(|p| p.iter().all(|v| v.is_finite() && v.abs() < 1e6))
            .map(|p| p.map(|n| n as f32))
            .unwrap_or([-2.; 2])
    }
    pub(crate) fn covers(&self, bounds: [f64; 4]) -> Result<(), String> {
        if let Some(w) = &self.warp {
            w.covers(bounds)?;
        }
        Ok(())
    }
    pub(crate) fn bounds(&self, bounds: [f64; 4]) -> Result<[f64; 4], String> {
        self.after.bounds(if let Some(w) = &self.warp {
            w.bounds(bounds)?
        } else {
            bounds
        })
    }
    pub(crate) fn polygon(&self, points: &[[f32; 2]]) -> Result<Vec<[f32; 2]>, String> {
        if !(3..=super::MAX_VERTICES).contains(&points.len()) {
            return Err("Warp polygon exceeds the 8192-vertex limit".into());
        }
        let original = points.iter().map(|p| p.map(f64::from)).collect::<Vec<_>>();
        let points = if let Some(w) = &self.warp {
            w.polygon(&original)?
        } else {
            original
        };
        let mut out = Vec::with_capacity(points.len());
        for point in points {
            let point = self.after.point(point)?.map(|p| p as f32);
            if point.iter().any(|v| !v.is_finite() || v.abs() > 100000.) {
                return Err("Warp geometry exceeds document coordinate limits".into());
            }
            if out.last() != Some(&point) {
                out.push(point);
            }
        }
        if out.len() > 1 && out.first() == out.last() {
            out.pop();
        }
        crate::selection::bounds(&out)?;
        Ok(out)
    }
}

fn native_bounds(layer: &Layer, raster: &crate::raster::Raster) -> Result<[f64; 4], String> {
    let frame = Map::affine_matrix([1., 0., 0., 1., layer.x as f64, layer.y as f64]);
    if let Some(original) = &raster.retained {
        original.support_bounds_in(frame)
    } else {
        crate::raster::check_size(raster.width, raster.height)?;
        raster.validate_layout()?;
        frame.bounds([
            -1.,
            -1.,
            raster.width as f64 + 1.,
            raster.height as f64 + 1.,
        ])
    }
}

/// Query only the original command targets; pending affine/P/W fields do not
/// redefine its input plane. Used by UI initialization/presets and engine admission.
pub(crate) fn domain(doc: &Document, commands: &[Value]) -> Result<[f64; 4], String> {
    if commands.is_empty() || commands.len() > 100 {
        return Err("Choose 1–100 Warp targets".into());
    }
    let mut bounds = Vec::with_capacity(commands.len() * 4);
    let mut seen = std::collections::HashSet::new();
    for c in commands {
        let root = doc
            .layers
            .iter()
            .find(|l| c["layer"].as_str() == Some(l.id.as_str()))
            .ok_or("Warp target no longer exists")?;
        // Match Engine's linked-mask command normalization before folder dispatch.
        let mask = c["mask"] == true && root.mask.as_ref().is_none_or(|m| !m.linked);
        let ids = if root.kind == "group" && !mask {
            crate::transform::tree_ids(doc, &[root.id.clone()])
        } else {
            vec![root.id.clone()]
        };
        let selected = doc.selection.filter(|_| c["selection_only"] != false);
        let mut selected_targets = 0;
        for layer in doc.layers.iter().filter(|l| ids.contains(&l.id)) {
            if !seen.insert((
                layer.id.as_str(),
                mask,
                if mask { c["step"].as_str() } else { None },
                selected.is_some(),
            )) {
                continue;
            }
            if layer.source.is_some() {
                return Err("Rasterize editable sources at native precision before Warp".into());
            }
            if !mask
                && root.kind == "group"
                && !matches!(layer.kind.as_str(), "group" | "paint" | "fill")
            {
                return Err(
                    "Rasterize adjustment children at native precision before a folder Warp".into(),
                );
            }
            if mask
                && layer.mask.as_ref().is_none_or(|m| {
                    !m.steps.iter().any(|s| {
                        s.kind == "paint" && c["step"].as_str().is_none_or(|id| id == s.id)
                    })
                })
            {
                return Err("Choose a paint mask step to Warp".into());
            }
            if selected.is_some() {
                if layer.kind == "group" {
                    if mask {
                        selected_targets += 1;
                    }
                    continue;
                }
                if !mask && !matches!(layer.kind.as_str(), "paint" | "fill") {
                    return Err("Warp selected raster pixels or a paint mask".into());
                }
                selected_targets += 1;
                continue;
            }
            if !mask && !matches!(layer.kind.as_str(), "group" | "adjustment") {
                bounds.push(native_bounds(layer, &layer.pixels)?);
            }
            if mask || layer.mask.as_ref().is_some_and(|m| m.linked) {
                let Some(m) = &layer.mask else {
                    if mask {
                        return Err("Warp target has no mask".into());
                    }
                    continue;
                };
                let mut count = 0;
                for step in &m.steps {
                    if mask
                        && (step.kind != "paint"
                            || c["step"].as_str().is_some_and(|id| id != step.id))
                    {
                        continue;
                    }
                    bounds.push(native_bounds(layer, &step.pixels)?);
                    count += 1;
                }
                if mask && count == 0 {
                    return Err("Choose a paint mask step to Warp".into());
                }
            }
        }
        if let Some(b) = selected {
            if selected_targets == 0 && bounds.is_empty() {
                return Err("Folder has no selected raster content to Warp".into());
            }
            bounds.push([
                b[0] as f64 - 1.,
                b[1] as f64 - 1.,
                b[2] as f64 + 1.,
                b[3] as f64 + 1.,
            ]);
        }
    }
    if bounds.is_empty() {
        return Err("Warp needs raster Color or a paint mask target".into());
    }
    let points = bounds
        .into_iter()
        .flat_map(|b| [[b[0], b[1]], [b[2], b[3]]])
        .collect::<Vec<_>>();
    let b = extrema(&points);
    let b = [
        b[0].floor() - 1.,
        b[1].floor() - 1.,
        b[2].ceil() + 1.,
        b[3].ceil() + 1.,
    ];
    projective::corners(b)?;
    Ok(b)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{engine::Engine, raster::Raster};
    use serde_json::json;

    #[test]
    fn shared_command_map_is_affine_after_projective_after_warp_with_one_inverse() {
        let grid = Grid::preset([0., 0., 16., 12.], "arc", 0.25).unwrap();
        let c = json!({"op":"transform","warp":grid,"projective":{"rect":[0,0,16,12],"quad":[[0,0],[12.8,0],[12.8,9.6],[0,12]]},"pivot":[0,0],"scale_x":1.5,"scale_y":0.75,"dx":3,"dy":1});
        let map = CommandMap::new(&c, [8., 6.]).unwrap();
        for p in [[0.0_f64, 0.], [4., 2.], [8., 6.], [13.25, 9.5], [16., 12.]] {
            let warped_y = p[1] + 3. * (1. - (p[0] / 8. - 1.).abs());
            let divisor = 1. + p[0] / 64.;
            let expected = [p[0] / divisor * 1.5 + 3., warped_y / divisor * 0.75 + 1.];
            let actual = map.point(p).unwrap();
            let restored = map.inverse(actual).unwrap();
            for a in 0..2 {
                assert!((actual[a] - expected[a]).abs() < 1e-10);
                assert!((restored[a] - p[a]).abs() < 1e-10);
            }
        }
        let polygon = map
            .polygon(&[[0., 0.], [16., 0.], [16., 12.], [0., 12.]])
            .unwrap();
        assert!(polygon.len() > 4);
        let b = map.bounds([0., 0., 16., 12.]).unwrap();
        for p in polygon {
            assert!(
                p[0] as f64 >= b[0]
                    && p[0] as f64 <= b[2]
                    && p[1] as f64 >= b[1]
                    && p[1] as f64 <= b[3]
            );
        }
    }

    #[test]
    fn target_domain_includes_hidden_off_frame_mask_originals_and_ignores_pending_maps() {
        for depth in [8, 16] {
            let mut e = Engine::new();
            e.doc = Document::new_depth(32, 24, depth).unwrap();
            let id = e.doc.layers[0].id.clone();
            e.doc.layers[0].pixels = Raster::new_depth(8, 6, depth);
            e.edit(
                "human",
                &[
                    json!({"op":"mask.add","layer":id}),
                    json!({"op":"mask.link","layer":id,"linked":false}),
                ],
                None,
                None,
                "Mask",
            )
            .unwrap();
            e.doc.layers[0].mask.as_mut().unwrap().steps[1]
                .pixels
                .set16(3, 2, [12347, 12347, 12347, 65535]);
            let step = e.doc.layers[0].mask.as_ref().unwrap().steps[1].id.clone();
            let target =
                json!({"op":"transform","layer":id,"mask":true,"step":step,"selection_only":false});
            e.edit(
                "human",
                &[json!({"op":"move","layer":id,"mask":true,"step":step,"dx":20})],
                None,
                None,
                "Off-frame mask",
            )
            .unwrap();
            assert_eq!(
                e.doc.layers[0].mask.as_ref().unwrap().steps[1]
                    .pixels
                    .bytes(),
                0
            );
            let bounds = domain(&e.doc, &[target.clone()]).unwrap();
            assert!(bounds[0] <= 19. && bounds[1] <= -1. && bounds[2] >= 29. && bounds[3] >= 7.);
            assert!(bounds[0] >= 17. && bounds[1] >= -3. && bounds[2] <= 31. && bounds[3] <= 9.);
            let before = serde_json::to_value(&e.doc).unwrap();
            let mut pending = target.clone();
            pending["angle"] = json!(45.);
            pending["warp"] = json!({"preset":"arc","rect":bounds,"amount":0.25});
            assert_eq!(domain(&e.doc, &[pending]).unwrap(), bounds);
            assert_eq!(serde_json::to_value(&e.doc).unwrap(), before);
            e.doc.selection = Some([2, 2, 6, 5]);
            let mut selected = target;
            selected["selection_only"] = json!(true);
            assert_eq!(domain(&e.doc, &[selected]).unwrap(), [0., 0., 8., 7.]);
        }
    }

    #[test]
    fn retained_stage_deserialization_never_builds_an_unbounded_stage_vector() {
        #[derive(serde::Deserialize)]
        struct Wrapper {
            #[serde(deserialize_with = "crate::warp::stages")]
            stages: Vec<super::super::Stage>,
        }
        let stage = super::super::Stage {
            before: Map::IDENTITY.matrix(),
            grid: Grid::identity([0., 0., 16., 12.]).unwrap(),
        };
        let valid =
            serde_json::from_value::<Wrapper>(json!({"stages":vec![stage.clone();16]})).unwrap();
        assert_eq!(valid.stages.len(), 16);
        assert!(serde_json::from_value::<Wrapper>(json!({"stages":vec![stage;17]})).is_err());
    }

    #[test]
    fn source_admission_counts_color_and_mask_grid_metadata_at_the_native_budget_boundary() {
        use crate::engine::{Layer, Mask, MaskStep};
        use std::sync::Arc;
        let mut doc = Document::new(2048, 2048).unwrap();
        let mut layer = Layer::new("Budget", "paint", 2048, 2048);
        // A single shared 256KiB tile represents an exact 512MiB logical raster
        // budget across32layers. This tests admission without allocating512MiB.
        let tile = Arc::new(vec![255; 256 * 256 * 4]);
        for y in 0..8 {
            for x in 0..8 {
                layer.pixels.tiles.insert((x, y), tile.clone());
            }
        }
        doc.layers = (0..32)
            .map(|_| {
                let mut l = layer.clone();
                l.id = crate::engine::id();
                l
            })
            .collect();
        assert_eq!(
            doc.layers
                .iter()
                .map(|l| l.pixels.stored_bytes())
                .sum::<usize>(),
            512 * 1024 * 1024
        );
        crate::psd::validate(&doc).unwrap();
        for mask in [false, true] {
            let mut candidate = doc.clone();
            let mut pixels = Raster::new(2048, 2048);
            let mut original = crate::retained::Original::from(&pixels);
            original.warps.push(super::super::Stage {
                before: Map::IDENTITY.matrix(),
                grid: Grid::identity([-2., -2., 2050., 2050.]).unwrap(),
            });
            original.validate(8).unwrap();
            pixels.retained = Some(Box::new(original));
            if mask {
                candidate.layers[0].mask = Some(Mask {
                    linked: false,
                    enabled: true,
                    cache_key: crate::engine::id(),
                    steps: vec![MaskStep {
                        id: crate::engine::id(),
                        kind: "paint".into(),
                        enabled: true,
                        weight: 1.,
                        value: 255.,
                        pixels,
                        settings: json!({}),
                    }],
                });
            } else {
                candidate.layers[0].pixels.retained = pixels.retained;
            }
            assert_eq!(
                crate::psd::validate(&candidate).unwrap_err(),
                "Document memory limit"
            );
        }
    }
}
