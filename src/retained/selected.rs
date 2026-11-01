//! One selected command prepares every pixel, selection and typed local clip
//! update before the existing guarded Engine/working-transaction publication.
#[path = "selected/effects.rs"]
mod effects;
#[path = "selected/layer.rs"]
mod layer;
#[path = "selected/state.rs"]
mod state;
use crate::{
    engine::native::{
        CapturedEdit, CommandToken, CoveragePatch, CoveragePixels, Method, NativeDocument,
        PreparedDelta, RasterBatch, SelectionPatch,
    },
    large_document::Error,
    raster::Surface,
    retained::transaction::{self, Session},
    transform::native::{
        selected::{self, Placement, Selection},
        Work,
    },
};
use std::cell::RefCell;

pub(crate) fn prepare(
    captured: CapturedEdit,
    token: CommandToken,
    session: &impl Session,
    work: &mut Work,
    check: &dyn Fn() -> Result<(), String>,
) -> Result<PreparedDelta, String> {
    let base = captured.document().clone();
    let command = captured
        .descriptor()
        .ok_or("Selected transform needs a captured descriptor")?
        .commands()
        .find(|c| c.token() == token)
        .ok_or("Selected transform command belongs to another descriptor")?;
    if command.method() != Method::Geometry
        || !matches!(
            command.parameters()["op"].as_str(),
            Some("move" | "transform")
        )
    {
        return Err("Selected transform needs a move or transform command".into());
    }
    let cancellation = captured.cancellation();
    let checked = || {
        cancellation.check()?;
        session.check()?;
        check()?;
        cancellation.check()
    };
    checked()?;
    if !base.budget().same_pool(&session.resident())
        || !base.disk_budget().same_pool(&session.disk())
    {
        return Err("Selected transform session uses another admission".into());
    }
    let id = command.parameters()["layer"]
        .as_str()
        .ok_or("Selected transform needs a layer")?;
    let root = base
        .metadata()
        .layers()
        .find(|l| l.id() == id)
        .ok_or("Unknown selected transform layer")?;
    let mask = command.parameters()["mask"] == true && root.mask().is_none_or(|m| !m.linked());
    // Engine applies linked mask commands to the layer before folder dispatch.
    let folder = root.kind() == "group" && !mask;
    let selection = if root.kind() == "group" {
        Selection::for_folder(&base)?
    } else {
        Selection::new(&base)?
    };
    let count = base.metadata().layers().len();
    let list_bytes = count
        .checked_mul(std::mem::size_of::<crate::engine::native::LayerMetadata<'_>>() + 128)
        .and_then(|n| n.checked_add(4096))
        .ok_or("Selected target list overflow")?;
    let list_charge = base
        .budget()
        .reserve_bytes(list_bytes, list_bytes)
        .map_err(|e| e.to_string())?;
    let mut targets = Vec::new();
    targets
        .try_reserve_exact(count)
        .map_err(|_| "Selected target list allocation refused")?;
    for node in base.metadata().layers() {
        checked()?;
        if if folder {
            super::planning::belongs(&base, node, id, work)?
        } else {
            node.id() == id
        } {
            if node.locked() {
                return Err(if folder {
                    "A layer inside the folder is locked"
                } else {
                    "Unlock the layer or its folder before changing pixels"
                }
                .into());
            }
            if folder && node.kind() == "group" {
                continue;
            }
            if folder && (!matches!(node.kind(), "paint" | "fill") || node.source().is_some()) {
                return Err("Rasterize editable text/vector or adjustment children before transforming selected folder pixels".into());
            }
            if node.source().is_some() {
                return Err("Transform the complete editable text/vector layer with selection_only:false, or rasterize it for a pixel/mask transform".into());
            }
            if node.kind() == "adjustment" {
                return Err("Edit the adjustment effect settings or paint on its mask".into());
            }
            targets.push(node);
        }
    }
    if targets.is_empty() {
        return Err("Folder has no raster content to transform".into());
    }
    if targets.capacity() * std::mem::size_of::<crate::engine::native::LayerMetadata<'_>>()
        > list_charge.bytes()
    {
        return Err("Selected target list exceeds admission".into());
    }
    let mut planes = 0usize;
    let mut effect_count = 0usize;
    let mut effect_bytes = 0usize;
    for target in &targets {
        planes = planes
            .checked_add(1 + target.mask().map_or(0, |m| m.steps().len()))
            .ok_or("Selected plane count overflow")?;
        for effect in target.effects().filter(|e| e.kind() == "liquify") {
            effect_count += 1;
            effect_bytes = effect_bytes
                .checked_add(effect.id().len())
                .and_then(|n| {
                    effect
                        .clips()
                        .len()
                        .checked_mul(
                            std::mem::size_of::<crate::engine::native::EffectClipPatch>() + 128,
                        )
                        .and_then(|m| n.checked_add(m))
                })
                .and_then(|n| n.checked_add(4096))
                .ok_or("Selected effect envelope overflow")?;
        }
    }
    let bytes = planes
        .checked_mul(64 * 1024)
        .and_then(|n| n.checked_add(effect_bytes))
        .and_then(|n| {
            targets
                .len()
                .checked_mul(4096)
                .and_then(|m| n.checked_add(m))
        })
        .and_then(|n| n.checked_add(4096))
        .ok_or("Selected packet envelope overflow")?;
    let charge = base
        .budget()
        .reserve_bytes(bytes, bytes)
        .map_err(|e| e.to_string())?;
    let mut states = Vec::new();
    states
        .try_reserve_exact(targets.len())
        .map_err(|_| "Selected state allocation refused")?;
    let mut batch = RasterBatch {
        rasters: Vec::new(),
        layers: Vec::new(),
        effects: Vec::new(),
        selection: None,
    };
    batch
        .rasters
        .try_reserve_exact(planes)
        .map_err(|_| "Selected raster packet allocation refused")?;
    batch
        .layers
        .try_reserve_exact(targets.len())
        .map_err(|_| "Selected layer packet allocation refused")?;
    batch
        .effects
        .try_reserve_exact(effect_count)
        .map_err(|_| "Selected effect packet allocation refused")?;
    let failure = RefCell::new(None);
    let source_check = || match checked() {
        Ok(()) => Ok(()),
        Err(reason) => {
            *failure.borrow_mut() = Some(reason);
            Err(Error::Cancelled)
        }
    };
    // Empty raw coverage returns before layer/mask validation in the resident
    // selection_with_coverage path. Geometric single-layer bounds still validate.
    let empty = selection.rasterized() && selection.bounds[0] >= selection.bounds[2];
    let placement = if empty {
        None
    } else {
        Some(Placement::for_selection(
            &selection,
            command.parameters(),
            base.budget(),
        )?)
    };
    let staged = (|| {
        if let Some(placement) = &placement {
            let write_box = [
                placement.area[0].min(placement.bounds[0]),
                placement.area[1].min(placement.bounds[1]),
                placement.area[2].max(placement.bounds[2]),
                placement.area[3].max(placement.bounds[3]),
            ];
            for target in &targets {
                let linked = target.mask().is_some_and(|m| m.linked());
                if linked || !mask {
                    command.check_write(&base, target.pixels().slot(), write_box)?;
                }
                if linked || mask {
                    if let Some(m) = target.mask() {
                        for step in m.steps().filter(|s| {
                            s.kind() == "paint"
                                && (linked
                                    || command.parameters()["step"]
                                        .as_str()
                                        .is_none_or(|id| id == s.id()))
                        }) {
                            command.check_write(&base, step.pixels().slot(), write_box)?;
                        }
                    }
                }
                if crate::warp::parse(command.parameters())?.is_some() {
                    let (_, frame) = layer::after(
                        target.origin(),
                        [target.pixels().width(), target.pixels().height()],
                        placement,
                    )?;
                    let steps = target.mask().map_or(0, |m| m.steps().len());
                    crate::warp::work_budget(
                        frame[0],
                        frame[1],
                        1 + steps * (1 + steps),
                        1,
                        selection.polygon().map_or(4, |p| p.len()) + placement.polygon.len(),
                    )?;
                }
                states.push(layer::Layer::new(&base, *target)?);
                let state = states.last_mut().ok_or("Missing selected working layer")?;
                state.apply(
                    &base,
                    &selection,
                    placement,
                    command.parameters(),
                    mask,
                    session,
                    work,
                    &checked,
                )?;
                state.reconcile(&base, session, work, &checked)?;
                state.check_reframed_writes(&base, command, work, &checked)?;
                if state.expanded() {
                    batch.effects.extend(effects::patches(
                        &base,
                        state.metadata,
                        state.offset(),
                        &checked,
                    )?);
                }
            }
        }
        let previous = base
            .metadata()
            .selection_previous()
            .map(|s| effects::coverage(&base, s, [0, 0], &checked))
            .transpose()?;
        let (bounds, polygon, current) = if selection.rasterized() {
            let current = if let Some(placement) = &placement {
                scalar(
                    &base,
                    &selection,
                    placement,
                    session,
                    work,
                    &checked,
                    &source_check,
                )?
            } else if let Some(source) = base
                .metadata()
                .selection_coverage()
                .filter(|m| m.bounds() == selection.bounds)
            {
                effects::coverage(&base, source, [0, 0], &checked)?
            } else {
                empty_coverage(&base, selection.bounds, session, &checked)?
            };
            (Some(current.bounds), None, Some(current))
        } else {
            let placement = placement.as_ref().ok_or("Missing selected geometry")?;
            let polygon = base
                .copy_contour_metadata(std::slice::from_ref(&placement.polygon), &checked)?
                .map_unique(|mut p, _| p.pop().ok_or_else(|| "Missing selected polygon".into()))?;
            let current = base
                .metadata()
                .selection_coverage()
                .map(|m| effects::coverage(&base, m, [0, 0], &checked))
                .transpose()?;
            (Some(placement.bounds), Some(polygon), current)
        };
        batch.selection = Some(SelectionPatch {
            bounds,
            polygon,
            current,
            previous,
        });
        checked()
    })();
    if let Err(reason) = staged {
        let mut reason = failure.into_inner().unwrap_or(reason);
        for state in states {
            if let Err(cleanup) = state.abort(session) {
                reason = format!("{reason}; selected plane retained: {cleanup}");
            }
        }
        if let Err(cleanup) = batch.abort(session, &|| Ok(())) {
            reason = format!("{reason}; selected packet retained: {cleanup}");
        }
        return Err(reason);
    }
    for state in states {
        state.pack(&mut batch);
    }
    let result = match captured.batch(batch, &checked) {
        Ok(prepared) => Ok(prepared),
        Err(refused) => {
            let reason = refused.reason().to_owned();
            match refused.abort(session, &|| Ok(())) {
                Ok(()) => Err(reason),
                Err(cleanup) => Err(format!("{reason}; selected packet retained: {cleanup}")),
            }
        }
    };
    drop(charge);
    drop(targets);
    drop(list_charge);
    result
}
fn scalar(
    base: &NativeDocument,
    selection: &Selection<'_>,
    placement: &Placement,
    session: &impl Session,
    work: &mut Work,
    check: &dyn Fn() -> Result<(), String>,
    source_check: &dyn Fn() -> crate::large_document::Result<()>,
) -> Result<CoveragePatch, String> {
    let mut metadata = None;
    let output = transaction::stage(base, session, check, |store, _| {
        let source = selected::write_coverage(selection, placement, store, work, source_check)?;
        let contours = base.copy_contour_metadata(source.contours(), check)?;
        metadata = Some((source.bounds(), source.origin(), contours));
        let mask = source.mask().clone();
        drop(source);
        Ok(mask)
    })?;
    let (bounds, origin, contours) = metadata.ok_or("Missing transformed coverage metadata")?;
    Ok(CoveragePatch {
        pixels: CoveragePixels::Output(output),
        bounds,
        origin,
        contours,
    })
}
fn empty_coverage(
    base: &NativeDocument,
    bounds: [i32; 4],
    session: &impl Session,
    check: &dyn Fn() -> Result<(), String>,
) -> Result<CoveragePatch, String> {
    let mut line = Vec::new();
    line.try_reserve_exact(4)
        .map_err(|_| "Empty coverage contour allocation refused")?;
    line.extend([
        [bounds[0] as f32, bounds[1] as f32],
        [bounds[2] as f32, bounds[1] as f32],
        [bounds[2] as f32, bounds[3] as f32],
        [bounds[0] as f32, bounds[3] as f32],
    ]);
    let contours = base.copy_contour_metadata(std::slice::from_ref(&line), check)?;
    let output = transaction::stage(base, session, check, |_, _| {
        Surface::zero(base.metadata().width(), base.metadata().height(), 8)
            .map_err(|e| e.to_string())
    })?;
    Ok(CoveragePatch {
        pixels: CoveragePixels::Output(output),
        bounds,
        origin: [0, 0],
        contours,
    })
}
