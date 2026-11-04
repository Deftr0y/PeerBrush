//! Retained transform policy over borrowed immutable native metadata.
use super::{pose::Pose, projection::Original};
use crate::{
    engine::{
        native::{LayerMetadata, NativeDocument, RasterMetadata, RasterSlot},
        scene::Source36,
    },
    large_document::{Reservation, Result as SourceResult},
    projective::Map,
    transform::native::Work,
};
use serde_json::Value;
#[path = "planning/folder.rs"]
mod folder;
pub(super) use folder::belongs;
pub(super) use folder::folder;
#[cfg(test)]
#[path = "planning/tests.rs"]
pub(super) mod tests;

pub(crate) enum Pixels<'a> {
    Native {
        input: Original<'a>,
        pose: Pose,
    },
    Fill {
        input: Original<'a>,
        pose: Pose,
        color: crate::raster::Pixel,
    },
    Zero {
        depth: u16,
    },
    Editable {
        source: &'a Source36,
        matrix: [f32; 6],
        depth: u16,
    },
}
pub(crate) struct Plane<'a> {
    pub target: RasterSlot,
    pub pixels: Pixels<'a>,
}
pub(crate) struct LayerPlan<'a> {
    pub layer: LayerMetadata<'a>,
    pub frame: [u32; 2],
    pub origin: [i32; 2],
    pub color: Option<Plane<'a>>,
    pub masks: Vec<Plane<'a>>,
    pub materialize_fill: bool,
    // Keep all Vec payloads above inseparable from their provisional charge.
    _charge: Reservation,
}
pub(super) fn unchanged<'a>(
    document: &'a NativeDocument,
    layer: LayerMetadata<'a>,
) -> Result<LayerPlan<'a>, String> {
    let (masks, charge) = charged_planes(document, 0)?;
    Ok(LayerPlan {
        layer,
        frame: [layer.pixels().width(), layer.pixels().height()],
        origin: layer.origin(),
        color: None,
        masks,
        materialize_fill: false,
        _charge: charge,
    })
}
fn lookup<'a>(document: &'a NativeDocument, id: &str) -> Result<LayerMetadata<'a>, String> {
    document
        .metadata()
        .layers()
        .find(|layer| layer.id() == id)
        .ok_or_else(|| "Unknown native transform layer".into())
}
fn plain_mask(layer: LayerMetadata<'_>) -> Result<(), String> {
    if layer.mask().is_some_and(|mask| {
        mask.steps()
            .any(|step| !matches!(step.kind(), "fill" | "paint"))
    }) {
        return Err("Independent or selected linked transforms require a fill/paint mask; mask effects are preserved unchanged. Unlink and choose a paint step for a mask-only transform.".into());
    }
    Ok(())
}
fn source<'a>(
    document: &'a NativeDocument,
    metadata: RasterMetadata<'a>,
) -> Result<(Original<'a>, Pose), String> {
    let input = Original::raster(document, metadata)?;
    let pixels = input.source()?;
    let pose = Pose::new(
        [pixels.width(), pixels.height()],
        pixels.depth(),
        input.metadata(),
        document.budget(),
    )?;
    Ok((input, pose))
}
fn original_bounds(
    input: &Original<'_>,
    pose: &Pose,
    full: bool,
    work: &mut Work,
    check: &dyn Fn() -> SourceResult<()>,
) -> Result<[f64; 4], String> {
    let pixels = input.source()?;
    let frame = [0, 0, pose.frame[0] as i32, pose.frame[1] as i32];
    let bounds = if full {
        frame
    } else {
        crate::transform::native::source_bounds(
            pixels,
            input.document().budget(),
            input.document().disk_budget(),
            work,
            check,
        )?
        .unwrap_or(frame)
    };
    pose.bounds(bounds)
}
fn union(a: Option<[f64; 4]>, b: [f64; 4]) -> Option<[f64; 4]> {
    Some(a.map_or(b, |a| {
        [
            a[0].min(b[0]),
            a[1].min(b[1]),
            a[2].max(b[2]),
            a[3].max(b[3]),
        ]
    }))
}
fn charged_planes(
    document: &NativeDocument,
    count: usize,
) -> Result<(Vec<Plane<'_>>, Reservation), String> {
    let bytes = count
        .checked_mul(std::mem::size_of::<Plane<'_>>() + 128)
        .and_then(|n| n.checked_add(4096))
        .ok_or("Native transform metadata overflow")?;
    let charge = document
        .budget()
        .reserve_bytes(bytes, bytes)
        .map_err(|e| e.to_string())?;
    let mut planes = Vec::new();
    planes
        .try_reserve_exact(count)
        .map_err(|_| "Native transform metadata allocation refused")?;
    if planes
        .capacity()
        .checked_mul(std::mem::size_of::<Plane<'_>>())
        .is_none_or(|n| n > charge.bytes())
    {
        return Err("Native transform metadata exceeds admission".into());
    }
    Ok((planes, charge))
}

/// Whole-layer and mask-only policy. Selected-pixel and folder dispatch build
/// on these plans, then stage every changed plane in one engine batch.
pub(crate) fn layer<'a>(
    document: &'a NativeDocument,
    id: &str,
    command: &Value,
    work: &mut Work,
    check: &dyn Fn() -> SourceResult<()>,
) -> Result<Option<LayerPlan<'a>>, String> {
    layer_with_pivot(document, id, command, None, work, check)
}

fn layer_with_pivot<'a>(
    document: &'a NativeDocument,
    id: &str,
    command: &Value,
    default_pivot: Option<[f64; 2]>,
    work: &mut Work,
    check: &dyn Fn() -> SourceResult<()>,
) -> Result<Option<LayerPlan<'a>>, String> {
    check().map_err(|e| e.to_string())?;
    let layer = lookup(document, id)?;
    if layer.source().is_some() {
        if command["op"] == "transform"
            && (crate::projective::parse(command)?.is_some()
                || crate::warp::parse(command)?.is_some())
        {
            return Err("Rasterize the editable text/vector source at its native precision before Distort, Perspective or Warp".into());
        }
        let independent_mask =
            command["mask"] == true && layer.mask().is_none_or(|mask| !mask.linked());
        if independent_mask
            || (document.metadata().selection().is_some() && command["selection_only"] != false)
        {
            return Err("Transform the complete editable text/vector layer with selection_only:false, or rasterize it for a pixel/mask transform".into());
        }
    }
    if command["mask"] == true && layer.mask().is_none_or(|mask| !mask.linked()) {
        return mask(document, layer, command).map(Some);
    }
    let unlinked = layer.mask().is_some_and(|mask| !mask.linked());
    if unlinked {
        plain_mask(layer)?;
    }
    let [x, y] = layer.origin();
    let pixels = layer.pixels();
    let current_frame = [pixels.width(), pixels.height()];
    let count = layer.mask().map_or(0, |mask| mask.steps().len());
    let (mut masks, charge) = charged_planes(document, count)?;
    if command["op"] == "move" {
        crate::transform::fractional_translation(command)?;
        let nx = x as f64 + command["dx"].as_f64().unwrap_or(0.);
        let ny = y as f64 + command["dy"].as_f64().unwrap_or(0.);
        if !nx.is_finite() || !ny.is_finite() || nx.abs() > 100000. || ny.abs() > 100000. {
            return Err("Move is outside the initial coordinate limits".into());
        }
        let origin = if layer.kind() == "adjustment" && layer.mask().is_none() {
            [0, 0]
        } else {
            [nx.round() as i32, ny.round() as i32]
        };
        if unlinked && origin != [x, y] {
            reframe(document, layer, current_frame, origin, &mut masks)?;
        }
        return Ok(Some(LayerPlan {
            layer,
            frame: current_frame,
            origin,
            color: None,
            masks,
            materialize_fill: false,
            _charge: charge,
        }));
    }
    let projective = crate::projective::parse(command)?;
    let warp = crate::warp::parse(command)?;
    let center = [
        x as f64 + current_frame[0] as f64 / 2.,
        y as f64 + current_frame[1] as f64 / 2.,
    ];
    let warped = warp
        .as_ref()
        .map(|_| crate::warp::CommandMap::new(command, center))
        .transpose()?;
    let (sx, sy) = crate::transform::scales(command)?;
    let angle = command["angle"].as_f64().unwrap_or(0.);
    let [dx, dy] = crate::transform::translation(command)?;
    let px = command["pivot"][0]
        .as_f64()
        .unwrap_or(default_pivot.unwrap_or(center)[0]);
    let py = command["pivot"][1]
        .as_f64()
        .unwrap_or(default_pivot.unwrap_or(center)[1]);
    if ![angle, px, py, dx, dy]
        .iter()
        .all(|v| v.is_finite() && v.abs() <= 100000.)
    {
        return Err("Invalid transform".into());
    }
    let (sin, cos) = angle.to_radians().sin_cos();
    let [a, b, c, d] = crate::transform::skewed(
        [
            super::geometry::exact(cos) * sx,
            super::geometry::exact(sin) * sx,
            -super::geometry::exact(sin) * sy,
            super::geometry::exact(cos) * sy,
        ],
        command,
    )?;
    let mut delta = [
        a,
        b,
        c,
        d,
        px + a * (x as f64 - px) + c * (y as f64 - py),
        py + b * (x as f64 - px) + d * (y as f64 - py),
    ];
    if dx != 0. {
        delta[4] += dx;
    }
    if dy != 0. {
        delta[5] += dy;
    }
    let projective_delta = projective
        .map(|p| {
            crate::projective::affine(command, p.center())?
                .compose(p.map)?
                .compose(Map::affine_matrix([1., 0., 0., 1., x as f64, y as f64]))
        })
        .transpose()?;
    let world = matches!(layer.kind(), "group" | "adjustment");
    if world && (layer.mask().is_none() || unlinked) {
        return Ok(None);
    }
    if layer.source().is_some() && (projective.is_some() || warp.is_some()) {
        return Err("Rasterize the editable source at its native precision before Distort, Perspective or Warp".into());
    }
    let mut color = if !world && layer.source().is_none() {
        let (input, mut pose) = source(document, pixels)?;
        if layer.kind() == "fill" && warp.is_none() {
            pose.frame = current_frame;
        }
        Some(if layer.kind() == "fill" {
            Pixels::Fill {
                input,
                pose,
                color: layer.color(),
            }
        } else {
            Pixels::Native { input, pose }
        })
    } else {
        None
    };
    let apply = |pose: &mut Pose| -> Result<(), String> {
        if let (Some(grid), Some(map)) = (&warp, &warped) {
            pose.append_warp(
                grid,
                Map::affine_matrix([1., 0., 0., 1., x as f64, y as f64]),
                map.after,
            )
        } else if let Some(map) = projective_delta {
            pose.prepend(map)
        } else {
            pose.matrix = super::compose(delta, pose.matrix);
            Ok(())
        }
    };
    if let Some(Pixels::Native { pose, .. } | Pixels::Fill { pose, .. }) = &mut color {
        apply(pose)?;
    }
    if !unlinked {
        if let Some(mask) = layer.mask() {
            for step in mask.steps() {
                let (input, mut pose) = source(document, step.pixels())?;
                apply(&mut pose)?;
                masks.push(Plane {
                    target: step.pixels().slot(),
                    pixels: Pixels::Native { input, pose },
                });
            }
        }
    }
    let mut full = false;
    if !unlinked {
        if let Some(mask) = layer.mask() {
            for step in mask.steps() {
                if step.kind() == "paint"
                    && document
                        .source(step.pixels().slot())?
                        .physical_tiles()
                        .next()
                        .is_some()
                {
                    full = true;
                }
            }
        }
    }
    let mut bounds = match &color {
        Some(Pixels::Native { input, pose }) => {
            Some(original_bounds(input, pose, full, work, check)?)
        }
        Some(Pixels::Fill { pose, .. }) => {
            Some(pose.bounds([0, 0, pose.frame[0] as i32, pose.frame[1] as i32])?)
        }
        _ => None,
    };
    if !unlinked {
        if let Some(mask) = layer.mask() {
            for (step, plan) in mask.steps().zip(&masks) {
                if let Pixels::Native { input, pose } = &plan.pixels {
                    if step.kind() == "paint" && input.source()?.physical_tiles().next().is_some() {
                        bounds = union(bounds, original_bounds(input, pose, true, work, check)?);
                    }
                }
            }
        }
    }
    if layer.source().is_some() || bounds.is_none() {
        let mut pose = Pose::new(current_frame, pixels.depth(), None, document.budget())?;
        apply(&mut pose)?;
        bounds = union(
            bounds,
            pose.bounds([0, 0, current_frame[0] as i32, current_frame[1] as i32])?,
        );
    }
    let mut bounds = bounds.ok_or("Native transform has no frame")?;
    let plain = |pixels: &Pixels<'_>| match pixels {
        Pixels::Native { pose, .. } | Pixels::Fill { pose, .. } => {
            pose.projective.is_none() && pose.warps().is_empty()
        }
        _ => true,
    };
    if projective_delta.is_none()
        && warp.is_none()
        && color.as_ref().is_none_or(plain)
        && masks.iter().all(|p| plain(&p.pixels))
    {
        bounds = bounds.map(super::geometry::exact);
    }
    let [left, top, right, bottom] = [
        bounds[0].floor(),
        bounds[1].floor(),
        bounds[2].ceil(),
        bounds[3].ceil(),
    ];
    if [left, top, right, bottom]
        .iter()
        .any(|v| !v.is_finite() || v.abs() > 100000.)
    {
        return Err("Transform is outside the coordinate limits".into());
    }
    let frame = [(right - left) as u32, (bottom - top) as u32];
    crate::raster::check_size(frame[0], frame[1])?;
    let mut populated = 0;
    for plan in &masks {
        if let Pixels::Native { input, .. } = &plan.pixels {
            populated += usize::from(input.source()?.physical_tiles().next().is_some());
        }
    }
    crate::transform::work_budget(frame[0], frame[1], 1 + populated)?;
    let stages = color
        .as_ref()
        .map_or(0, |p| match p {
            Pixels::Native { pose, .. } | Pixels::Fill { pose, .. } => pose.warps().len(),
            _ => 0,
        })
        .max(
            masks
                .iter()
                .map(|p| match &p.pixels {
                    Pixels::Native { pose, .. } => pose.warps().len(),
                    _ => 0,
                })
                .max()
                .unwrap_or(0),
        );
    if stages > 0 {
        crate::warp::work_budget(frame[0], frame[1], 1 + populated, stages, 0)?;
    }
    let origin = [left as i32, top as i32];
    let local = [1., 0., 0., 1., -left, -top];
    if let Some(Pixels::Native { pose, .. } | Pixels::Fill { pose, .. }) = &mut color {
        pose.matrix = super::compose(local, pose.matrix);
    }
    if !unlinked {
        if let Some(mask) = layer.mask() {
            for (step, plan) in mask.steps().zip(&mut masks) {
                let keep = step.kind() == "paint"
                    && match &plan.pixels {
                        Pixels::Native { input, .. } => {
                            input.source()?.physical_tiles().next().is_some()
                        }
                        _ => false,
                    };
                if keep {
                    if let Pixels::Native { pose, .. } = &mut plan.pixels {
                        pose.matrix = super::compose(local, pose.matrix);
                    }
                } else {
                    plan.pixels = Pixels::Zero {
                        depth: pixels.depth(),
                    };
                }
            }
        }
    }
    if unlinked {
        reframe(document, layer, frame, origin, &mut masks)?;
    }
    let color = if let Some(source) = layer.source() {
        Some(Plane {
            target: pixels.slot(),
            pixels: Pixels::Editable {
                source,
                matrix: super::policy::source_matrix_with_pivot(
                    [x, y],
                    current_frame,
                    origin,
                    source.matrix,
                    command,
                    default_pivot,
                )?,
                depth: pixels.depth(),
            },
        })
    } else {
        Some(Plane {
            target: pixels.slot(),
            pixels: color.unwrap_or(Pixels::Zero {
                depth: pixels.depth(),
            }),
        })
    };
    check().map_err(|e| e.to_string())?;
    Ok(Some(LayerPlan {
        layer,
        frame,
        origin,
        color,
        masks,
        materialize_fill: layer.kind() == "fill",
        _charge: charge,
    }))
}

fn reframe<'a>(
    document: &'a NativeDocument,
    layer: LayerMetadata<'a>,
    frame: [u32; 2],
    origin: [i32; 2],
    masks: &mut Vec<Plane<'a>>,
) -> Result<(), String> {
    if layer.origin() == origin
        && layer.mask().is_none_or(|mask| {
            mask.steps()
                .all(|step| [step.pixels().width(), step.pixels().height()] == frame)
        })
    {
        return Ok(());
    }
    if let Some(mask) = layer.mask() {
        crate::transform::work_budget(frame[0], frame[1], mask.steps().len())?;
        let old = layer.origin();
        for step in mask.steps() {
            let (input, mut pose) = source(document, step.pixels())?;
            pose.matrix = super::compose(
                [
                    1.,
                    0.,
                    0.,
                    1.,
                    f64::from(old[0] - origin[0]),
                    f64::from(old[1] - origin[1]),
                ],
                pose.matrix,
            );
            masks.push(Plane {
                target: step.pixels().slot(),
                pixels: Pixels::Native { input, pose },
            });
        }
    }
    Ok(())
}

fn mask<'a>(
    document: &'a NativeDocument,
    layer: LayerMetadata<'a>,
    command: &Value,
) -> Result<LayerPlan<'a>, String> {
    let projective = crate::projective::combined(command)?;
    let warp = crate::warp::parse(command)?;
    let [x, y] = layer.origin();
    let pixels = layer.pixels();
    let frame = [pixels.width(), pixels.height()];
    let warped = warp
        .as_ref()
        .map(|_| {
            crate::warp::CommandMap::new(
                command,
                [
                    x as f64 + frame[0] as f64 / 2.,
                    y as f64 + frame[1] as f64 / 2.,
                ],
            )
        })
        .transpose()?;
    let moving = command["op"] == "move";
    let (sx, sy) = if moving {
        (1., 1.)
    } else {
        crate::transform::scales(command)?
    };
    let angle = if moving {
        0.
    } else {
        command["angle"].as_f64().unwrap_or(0.)
    };
    let [dx, dy] = if moving {
        [
            command["dx"].as_f64().unwrap_or(0.).round(),
            command["dy"].as_f64().unwrap_or(0.).round(),
        ]
    } else {
        crate::transform::translation(command)?
    };
    let px = command["pivot"][0]
        .as_f64()
        .unwrap_or(x as f64 + frame[0] as f64 / 2.)
        - x as f64;
    let py = command["pivot"][1]
        .as_f64()
        .unwrap_or(y as f64 + frame[1] as f64 / 2.)
        - y as f64;
    if ![angle, px, py, dx, dy]
        .iter()
        .all(|v| v.is_finite() && v.abs() <= 100000.)
    {
        return Err("Invalid mask transform".into());
    }
    if command["step"].is_null() {
        plain_mask(layer)?;
    }
    let mask = layer.mask().ok_or("Layer has no mask")?;
    let count = mask
        .steps()
        .filter(|step| {
            step.kind() == "paint" && command["step"].as_str().is_none_or(|id| id == step.id())
        })
        .count();
    if !command["step"].is_null() && count == 0 {
        return Err("Select a paint mask step to transform".into());
    }
    crate::transform::work_budget(frame[0], frame[1], count)?;
    let (mut masks, charge) = charged_planes(document, count)?;
    let (sin, cos) = angle.to_radians().sin_cos();
    let [a, b, c, d] = crate::transform::skewed(
        [
            super::geometry::exact(cos) * sx,
            super::geometry::exact(sin) * sx,
            -super::geometry::exact(sin) * sy,
            super::geometry::exact(cos) * sy,
        ],
        command,
    )?;
    for step in mask.steps().filter(|step| {
        step.kind() == "paint" && command["step"].as_str().is_none_or(|id| id == step.id())
    }) {
        let (input, mut pose) = source(document, step.pixels())?;
        if let (Some(grid), Some(map)) = (&warp, &warped) {
            pose.append_warp(
                grid,
                Map::affine_matrix([1., 0., 0., 1., x as f64, y as f64]),
                Map::affine_matrix([1., 0., 0., 1., -(x as f64), -(y as f64)])
                    .compose(map.after)?,
            )?;
        } else if let Some(map) = projective {
            pose.prepend(
                Map::affine_matrix([1., 0., 0., 1., -(x as f64), -(y as f64)])
                    .compose(map)?
                    .compose(Map::affine_matrix([1., 0., 0., 1., x as f64, y as f64]))?,
            )?;
        } else {
            pose.matrix = super::compose(
                [
                    a,
                    b,
                    c,
                    d,
                    px - a * px - c * py + dx,
                    py - b * px - d * py + dy,
                ],
                pose.matrix,
            );
        }
        masks.push(Plane {
            target: step.pixels().slot(),
            pixels: Pixels::Native { input, pose },
        });
    }
    let stages = masks
        .iter()
        .map(|p| match &p.pixels {
            Pixels::Native { pose, .. } => pose.warps().len(),
            _ => 0,
        })
        .max()
        .unwrap_or(0);
    if stages > 0 {
        crate::warp::work_budget(frame[0], frame[1], count, stages, 0)?;
    }
    Ok(LayerPlan {
        layer,
        frame,
        origin: [x, y],
        color: None,
        masks,
        materialize_fill: false,
        _charge: charge,
    })
}
