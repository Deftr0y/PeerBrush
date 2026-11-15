use super::{Grid, Mapping};
use crate::{
    engine::{
        view::{DocumentView, LayerView, MaskStepView},
        Layer,
    },
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
    /// An empty footprint is distinct from an uncertain inverse near a horizon.
    pub(crate) fn sample_bounds(&self, bounds: [f64; 4]) -> Option<Option<[f64; 4]>> {
        let bounds = self.inverse.sampling_bounds(bounds)?;
        Some(match &self.warp {
            Some(warp) => warp.inverse_bounds(bounds),
            None => Some(bounds),
        })
    }
    pub(crate) fn sample_cost(&self) -> u64 {
        if self.warp.is_some() { 10 } else { 1 }
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

fn include(bounds: &mut Option<[f64; 4]>, b: [f64; 4]) {
    *bounds = Some(bounds.map_or(b, |a| {
        [
            a[0].min(b[0]),
            a[1].min(b[1]),
            a[2].max(b[2]),
            a[3].max(b[3]),
        ]
    }));
}
fn plane_bounds(
    doc: DocumentView<'_>,
    layer: LayerView<'_>,
    step: Option<MaskStepView<'_>>,
    check: &dyn Fn() -> crate::large_document::Result<()>,
) -> Result<[f64; 4], String> {
    check().map_err(|e| e.to_string())?;
    match (doc, layer, step) {
        (DocumentView::Resident(_), LayerView::Resident(layer), None) => {
            native_bounds(layer, &layer.pixels)
        }
        (
            DocumentView::Resident(_),
            LayerView::Resident(layer),
            Some(MaskStepView::Resident(step)),
        ) => native_bounds(layer, &step.pixels),
        (DocumentView::Native(doc), LayerView::Native(layer), step) => {
            let raster = match step {
                None => layer.pixels(),
                Some(MaskStepView::Native(step)) => step.pixels(),
                _ => return Err("Warp mask differs from document storage".into()),
            };
            let original = crate::retained::projection::Original::raster(doc, raster)?;
            let [x, y] = layer.origin();
            original.support_bounds_in(
                Map::affine_matrix([1., 0., 0., 1., x as f64, y as f64]),
                check,
            )
        }
        _ => Err("Warp target differs from document storage".into()),
    }
}

/// Query only the original command targets; pending affine/P/W fields do not
/// redefine its input plane. Used by UI initialization/presets and engine admission.
pub(crate) fn domain<'a>(
    doc: impl Into<DocumentView<'a>>,
    commands: &[Value],
    check: &dyn Fn() -> crate::large_document::Result<()>,
) -> Result<[f64; 4], String> {
    let doc = doc.into();
    check().map_err(|e| e.to_string())?;
    if commands.is_empty() || commands.len() > 100 {
        return Err("Choose 1–100 Warp targets".into());
    }
    // The temporary target IDs and deduplication table own no source pixels.
    let _charge = if let DocumentView::Native(native) = doc {
        let bytes = doc.layers().try_fold(1024usize, |bytes, layer| {
            layer
                .id()
                .len()
                .checked_mul(3)
                .and_then(|ids| {
                    commands.len().checked_mul(128).and_then(|entries| {
                        ids.checked_add(256)
                            .and_then(|ids| entries.checked_add(ids))
                    })
                })
                .and_then(|entry| bytes.checked_add(entry))
                .ok_or("Warp target metadata exceeds admission")
        })?;
        Some(
            native
                .budget()
                .reserve_bytes(bytes, 64 * 1024 * 1024)
                .map_err(|e| e.to_string())?,
        )
    } else {
        None
    };
    let mut bounds: Option<[f64; 4]> = None;
    let mut seen = std::collections::HashSet::new();
    for c in commands {
        check().map_err(|e| e.to_string())?;
        let root = doc
            .layers()
            .find(|l| c["layer"].as_str() == Some(l.id()))
            .ok_or("Warp target no longer exists")?;
        // Match Engine's linked-mask command normalization before folder dispatch.
        let mask = c["mask"] == true && root.mask().is_none_or(|m| !m.linked());
        let ids = if root.kind() == "group" && !mask {
            crate::transform::tree_ids_view(doc, &[root.id().to_owned()])
        } else {
            vec![root.id().to_owned()]
        };
        let selected = doc.selection().filter(|_| c["selection_only"] != false);
        let mut selected_targets = 0;
        for layer in doc.layers().filter(|l| ids.iter().any(|id| id == l.id())) {
            check().map_err(|e| e.to_string())?;
            if !seen.insert((
                layer.id(),
                mask,
                if mask { c["step"].as_str() } else { None },
                selected.is_some(),
            )) {
                continue;
            }
            if layer.source().is_some() {
                return Err("Rasterize editable sources at native precision before Warp".into());
            }
            if !mask
                && root.kind() == "group"
                && !matches!(layer.kind(), "group" | "paint" | "fill")
            {
                return Err(
                    "Rasterize adjustment children at native precision before a folder Warp".into(),
                );
            }
            if mask
                && layer.mask().is_none_or(|m| {
                    !(0..m.step_count()).filter_map(|i| m.step(i)).any(|s| {
                        s.kind() == "paint" && c["step"].as_str().is_none_or(|id| id == s.id())
                    })
                })
            {
                return Err("Choose a paint mask step to Warp".into());
            }
            if selected.is_some() {
                if layer.kind() == "group" {
                    if mask {
                        selected_targets += 1;
                    }
                    continue;
                }
                if !mask && !matches!(layer.kind(), "paint" | "fill") {
                    return Err("Warp selected raster pixels or a paint mask".into());
                }
                selected_targets += 1;
                continue;
            }
            if !mask && !matches!(layer.kind(), "group" | "adjustment") {
                include(&mut bounds, plane_bounds(doc, layer, None, check)?);
            }
            if mask || layer.mask().is_some_and(|m| m.linked()) {
                let Some(m) = layer.mask() else {
                    if mask {
                        return Err("Warp target has no mask".into());
                    }
                    continue;
                };
                let mut count = 0;
                for step in (0..m.step_count()).filter_map(|i| m.step(i)) {
                    check().map_err(|e| e.to_string())?;
                    if mask
                        && (step.kind() != "paint"
                            || c["step"].as_str().is_some_and(|id| id != step.id()))
                    {
                        continue;
                    }
                    include(&mut bounds, plane_bounds(doc, layer, Some(step), check)?);
                    count += 1;
                }
                if mask && count == 0 {
                    return Err("Choose a paint mask step to Warp".into());
                }
            }
        }
        if let Some(b) = selected {
            if selected_targets == 0 && bounds.is_none() {
                return Err("Folder has no selected raster content to Warp".into());
            }
            include(
                &mut bounds,
                [
                    b[0] as f64 - 1.,
                    b[1] as f64 - 1.,
                    b[2] as f64 + 1.,
                    b[3] as f64 + 1.,
                ],
            );
        }
    }
    let b = bounds.ok_or("Warp needs raster Color or a paint mask target")?;
    check().map_err(|e| e.to_string())?;
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
#[path = "domain_tests.rs"]
mod domain_tests;
#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        engine::{Document, Engine},
        raster::Raster,
    };
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
            e.resident_state_mut().expect("resident fixture").document = Document::new_depth(32, 24, depth).unwrap();
            let id = e.resident_state().expect("resident fixture").document.layers[0].id.clone();
            e.resident_state_mut().expect("resident fixture").document.layers[0].pixels = Raster::new_depth(8, 6, depth);
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
            e.resident_state_mut().expect("resident fixture").document.layers[0].mask.as_mut().unwrap().steps[1]
                .pixels
                .set16(3, 2, [12347, 12347, 12347, 65535]);
            let step = e.resident_state().expect("resident fixture").document.layers[0].mask.as_ref().unwrap().steps[1].id.clone();
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
                e.resident_state().expect("resident fixture").document.layers[0].mask.as_ref().unwrap().steps[1]
                    .pixels
                    .bytes(),
                0
            );
            let bounds = domain(&e.resident_state().expect("resident fixture").document, &[target.clone()], &|| Ok(())).unwrap();
            assert!(bounds[0] <= 19. && bounds[1] <= -1. && bounds[2] >= 29. && bounds[3] >= 7.);
            assert!(bounds[0] >= 17. && bounds[1] >= -3. && bounds[2] <= 31. && bounds[3] <= 9.);
            let before = serde_json::to_value(&e.resident_state().expect("resident fixture").document).unwrap();
            let mut pending = target.clone();
            pending["angle"] = json!(45.);
            pending["warp"] = json!({"preset":"arc","rect":bounds,"amount":0.25});
            assert_eq!(domain(&e.resident_state().expect("resident fixture").document, &[pending], &|| Ok(())).unwrap(), bounds);
            assert_eq!(serde_json::to_value(&e.resident_state().expect("resident fixture").document).unwrap(), before);
            e.resident_state_mut().expect("resident fixture").document.selection = Some([2, 2, 6, 5]);
            let mut selected = target;
            selected["selection_only"] = json!(true);
            assert_eq!(
                domain(&e.resident_state().expect("resident fixture").document, &[selected], &|| Ok(())).unwrap(),
                [0., 0., 8., 7.]
            );
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
