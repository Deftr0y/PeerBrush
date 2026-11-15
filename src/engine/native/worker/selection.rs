//! Selection commands preserve current/previous coverage in one native batch.
use super::*;
use crate::{
    edit_bounds::input::SelectionInput, engine::scene::Content36, large_document::ScratchBytes,
    selection::geometry::Geometry,
};
#[path = "selection_pixels.rs"]
mod pixels;
#[cfg(test)]
#[path = "selection_tests.rs"]
mod tests;

fn existing(
    base: &NativeDocument,
    coverage: CoverageMetadata<'_>,
    check: &dyn Fn() -> Result<()>,
) -> Result<CoveragePatch> {
    Ok(CoveragePatch {
        pixels: CoveragePixels::Existing(coverage.mask().slot()),
        bounds: coverage.bounds(),
        origin: coverage.origin(),
        contours: copy_admitted(&coverage.contours(), base.budget(), true, check)?,
    })
}
pub(super) fn current(
    base: &NativeDocument,
    session: &SourceSession,
    check: &dyn Fn() -> Result<()>,
) -> Result<Option<CoveragePatch>> {
    let metadata = base.metadata();
    let Some(bounds) = metadata.selection() else {
        return Ok(None);
    };
    if let Some(coverage) = metadata
        .selection_coverage()
        .filter(|c| c.bounds() == bounds)
    {
        return existing(base, coverage, check).map(Some);
    }
    let selected = SelectionInput::new(base)?;
    let polygon = selected.polygon();
    let rectangle = [
        [bounds[0] as f32, bounds[1] as f32],
        [bounds[2] as f32, bounds[1] as f32],
        [bounds[2] as f32, bounds[3] as f32],
        [bounds[0] as f32, bounds[3] as f32],
    ];
    let contours = copy_admitted(
        &[polygon.unwrap_or(&rectangle)],
        base.budget(),
        false,
        check,
    )?;
    // Geometry snapshots keep their original bounds/contours, as resident current()
    // does; contour tracing must not reinterpret their cached description.
    let (width, height) = (metadata.width(), metadata.height());
    let [left, top, right, bottom] = crate::preview::regions::intersect(bounds, width, height);
    let empty = left == right || top == bottom;
    let output = crate::retained::transaction::stage(base, session, check, |store, checked| {
        if empty {
            return crate::raster::Surface::zero(width, height, 8).map_err(|e| e.to_string());
        }
        let (width, height) = ((right - left) as u32, (bottom - top) as u32);
        let mut writer = crate::selection::NativeRows::new(width, height, 8, store.clone())
            .map_err(|e| e.to_string())?;
        let bytes = width as usize * 4;
        let mut row = ScratchBytes::new(base.budget(), bytes, bytes).map_err(|e| e.to_string())?;
        if polygon.is_none() {
            row.fill(255);
        }
        for y in top..bottom {
            check()?;
            if let Some(points) = polygon {
                for (x, pixel) in (left..right).zip(row.chunks_exact_mut(4)) {
                    let inside = crate::selection::contains(points, x as f32 + 0.5, y as f32 + 0.5);
                    pixel.fill(if inside { 255 } else { 0 });
                }
            }
            writer
                .row8((y - top) as u32, &row, &mut || checked())
                .map_err(|e| e.to_string())?;
        }
        writer.finish(&mut || checked()).map_err(|e| e.to_string())
    })?;
    Ok(Some(CoveragePatch {
        pixels: CoveragePixels::Output(output),
        bounds,
        origin: if empty { [0, 0] } else { [left, top] },
        contours,
    }))
}

pub(super) fn render(
    base: &NativeDocument,
    session: &SourceSession,
    frame: [i32; 4],
    feather: u32,
    check: &dyn Fn() -> Result<()>,
    active: Option<&dyn Fn() -> Result<()>>,
    read: impl FnMut(u32, &mut [u8]) -> Result<()>,
    combine: impl FnMut(u32, &mut [u8]) -> Result<()>,
) -> Result<CoveragePatch> {
    let mut bounds = [0; 4];
    let mut origin = [0; 2];
    let mut contours = copy_admitted(&Vec::<Vec<[f32; 2]>>::new(), base.budget(), true, check)?;
    let output = crate::retained::transaction::stage(base, session, check, |store, checked| {
        let [left, top, right, bottom] = frame;
        let [width, height] = [base.metadata().width(), base.metadata().height()];
        if left >= right || top >= bottom {
            return crate::raster::Surface::zero(width, height, 8).map_err(|e| e.to_string());
        }
        let stopped = std::cell::RefCell::new(None);
        let pinned = || {
            active.unwrap_or(check)().map_err(|reason| {
                *stopped.borrow_mut() = Some(reason);
                crate::large_document::Error::Cancelled
            })
        };
        let coverage = crate::selection::stage_feathered_rows(
            (right - left) as u32,
            (bottom - top) as u32,
            feather,
            store.clone(),
            base.budget(),
            if active.is_some() { &pinned } else { checked },
            active.map(|_| checked),
            read,
            combine,
        )
        .map_err(|reason| stopped.borrow_mut().take().unwrap_or(reason))?;
        if coverage.bounds() == [0; 4] {
            return crate::raster::Surface::zero(width, height, 8).map_err(|e| e.to_string());
        }
        let coverage = if left != 0 || top != 0 {
            coverage
                .translate_unique([left, top])
                .map_err(|e| e.to_string())?
        } else {
            coverage
        };
        contours = copy_admitted(&coverage.contours(), base.budget(), true, check)?;
        bounds = coverage.bounds();
        origin = coverage.origin();
        Ok(coverage.mask().clone())
    })?;
    Ok(CoveragePatch {
        pixels: CoveragePixels::Output(output),
        bounds,
        origin,
        contours,
    })
}

pub(super) fn path_points(
    base: &NativeDocument,
    c: &serde_json::Value,
    check: &dyn Fn() -> Result<()>,
) -> Result<Admitted<Vec<[f32; 2]>>> {
    let id = c["path"].as_str().ok_or("Supply the source path ID")?;
    super::super::patch::unlocked(&base.core.metadata, id)?;
    let layer = base
        .metadata()
        .layers()
        .find(|layer| layer.id() == id)
        .ok_or("Unknown source path")?;
    let source = layer
        .source()
        .ok_or("Close an editable Pen path before converting its area")?;
    let Content36::Bezier {
        nodes,
        closed: true,
        ..
    } = source.definition()
    else {
        return Err("Close an editable Pen path before converting its area".into());
    };
    let charge = MetadataCharge::reserve(
        base.budget(),
        8192 * std::mem::size_of::<[f32; 2]>() + 4096,
        check,
    )?;
    let points = crate::path::transformed_polygon(nodes, source.matrix, layer.origin())?;
    if points.capacity() * std::mem::size_of::<[f32; 2]>() > charge.bytes() {
        return Err("Path polygon exceeds admission".into());
    }
    Ok(Admitted::from_parts(Arc::new(points), charge))
}

pub(super) fn prepare(
    edit: CapturedEdit,
    session: &SourceSession,
    check: &dyn Fn() -> Result<()>,
    check_active: &dyn Fn() -> Result<()>,
) -> Result<PreparedDelta> {
    let base = edit.document().clone();
    let cancellation = edit.cancellation();
    let guarded = || {
        cancellation.check()?;
        session.check()?;
        check()
    };
    let pinned = || {
        cancellation.check()?;
        check_active()?;
        session.check_pinned_work()
    };
    guarded()?;
    let command = edit
        .descriptor()
        .ok_or("Native selection descriptor missing")?
        .commands()
        .next()
        .ok_or("Native selection command missing")?;
    let token = command.token();
    let c = command.parameters();
    let op = c["op"].as_str().unwrap_or("");
    if op != "path.to_selection"
        && c.get("rect")
            .is_some_and(|r| !r.is_null() && crate::engine::rect(r).is_none())
    {
        return Err("Invalid rectangle coordinates".into());
    }
    if handler(op) != Some(Handler::Selection)
        || !(command.method() == Method::Selection
            || op == "path.to_selection" && command.method() == Method::Source)
    {
        return Err("Unsupported native selection producer".into());
    }
    let _packet = base
        .budget()
        .reserve_bytes(4096, 4096)
        .map_err(|e| e.to_string())?;
    if matches!(
        op,
        "selection.refine" | "selection.import" | "selection.wand_update"
    ) || op == "selection"
        && ["wand", "quick", "object"].contains(&c["kind"].as_str().unwrap_or(""))
    {
        if op == "selection" && !c["polygon"].is_null() {
            let points: Admitted<Vec<[f32; 2]>> =
                copy_admitted(&c["polygon"], base.budget(), false, &guarded)?;
            crate::selection::bounds(&points)?;
        }
        return pixels::prepare(edit, session, &guarded);
    }
    let reselect = op == "selection.reselect";
    let plain = op == "selection"
        && ["kind", "mode", "feather"]
            .iter()
            .all(|key| c.get(key).is_none());
    let clear = op == "selection.clear"
        || op == "selection"
            && c["rect"].is_null()
            && c["polygon"].is_null()
            && c["kind"].is_null();
    if reselect && base.metadata().selection_previous().is_none() {
        return edit.unchanged(token, &guarded);
    }
    let path_parameters = serde_json::json!({"kind":"polygon"});
    let points: Option<Admitted<Vec<[f32; 2]>>> = if op == "path.to_selection" {
        Some(path_points(&base, c, &guarded)?)
    } else if !clear && !reselect && !c["polygon"].is_null() {
        Some(copy_admitted(
            &c["polygon"],
            base.budget(),
            false,
            &guarded,
        )?)
    } else {
        None
    };
    let parameters = if op == "path.to_selection" {
        &path_parameters
    } else {
        c
    };
    let plain_bounds = if plain {
        if let Some(points) = &points {
            let bounds = crate::selection::bounds(points)?;
            if c.get("rect")
                .and_then(crate::engine::rect)
                .is_some_and(|rect| rect != bounds)
            {
                return Err("Selection rectangle must match its polygon bounds".into());
            }
            Some(bounds)
        } else {
            c.get("rect").and_then(crate::engine::rect)
        }
    } else {
        None
    };
    let geometry = if clear || reselect || plain {
        None
    } else {
        Some(Geometry::new(
            [base.metadata().width(), base.metadata().height()],
            parameters,
            points.as_deref().map(Vec::as_slice),
        )?)
    };
    let mode = if op == "selection.modify" {
        "replace"
    } else {
        c["mode"].as_str().unwrap_or("replace")
    };
    if !clear && !reselect && !["replace", "add", "subtract", "intersect"].contains(&mode) {
        return Err("Choose replace, add, subtract or intersect".into());
    }
    let feather = c["feather"]
        .as_f64()
        .filter(|n| n.is_finite())
        .unwrap_or(0.)
        .round()
        .clamp(0., 64.) as u32;
    let count = geometry.as_ref().map_or(0, Geometry::points);
    let charge = base
        .budget()
        .reserve_bytes(count * std::mem::size_of::<f32>(), 8192 * 4)
        .map_err(|e| e.to_string())?;
    let mut crossings = Vec::new();
    crossings
        .try_reserve_exact(count)
        .map_err(|_| "Selection crossing allocation refused")?;
    if crossings.capacity() * std::mem::size_of::<f32>() > charge.bytes() {
        return Err("Selection crossings exceed admission".into());
    }
    let selected = SelectionInput::new(&base)?;
    let remembered = base
        .metadata()
        .selection_previous()
        .map(|c| existing(&base, c, &guarded))
        .transpose()?;
    let previous = current(&base, session, &guarded)?;
    let mut batch = RasterBatch {
        rasters: Vec::new(),
        layers: Vec::new(),
        effects: Vec::new(),
        selection: Some(SelectionPatch {
            bounds: None,
            polygon: None,
            current: None,
            previous: if reselect {
                previous
            } else {
                previous.or(remembered)
            },
        }),
    };
    if plain {
        let selection = batch.selection.as_mut().unwrap();
        selection.bounds = plain_bounds;
        selection.polygon = points;
        return apply_batch(edit, batch, session, &guarded);
    }
    let prepared = (|| -> Result<Option<CoveragePatch>> {
        if clear {
            return Ok(None);
        }
        if reselect {
            return base
                .metadata()
                .selection_previous()
                .map(|c| existing(&base, c, &guarded))
                .transpose();
        }
        let (width, height) = (base.metadata().width(), base.metadata().height());
        let clip = |area| {
            let [l, t, r, b] = crate::preview::regions::intersect(area, width, height);
            (l < r && t < b).then_some([l, t, r, b])
        };
        let window = |area, reach| crate::preview::regions::window(area, reach, width, height);
        let overlap = |a: [i32; 4], b: [i32; 4]| {
            [a[0].max(b[0]), a[1].max(b[1]), a[2].min(b[2]), a[3].min(b[3])]
        };
        let prior = || selected.support().and_then(clip);
        let expand = c["mode"].as_str().unwrap_or("expand") == "expand";
        let radius = c["radius"]
            .as_f64()
            .filter(|n| n.is_finite())
            .unwrap_or(1.)
            .round()
            .clamp(1., 64.) as usize;
        let frame = if op == "selection.invert" {
            Some([0, 0, width as i32, height as i32])
        } else if op == "selection.modify" {
            let reach = feather as i32 + if expand { radius as i32 } else { 0 };
            prior().map(|area| window(area, reach))
        } else {
            let shape = geometry
                .as_ref()
                .and_then(|shape| clip(shape.bounds()))
                .map(|area| window(area, feather as i32));
            match mode {
                "add" => match (prior(), shape) {
                    (Some(a), Some(b)) => Some(crate::engine::reframe_scope::union(a, b)),
                    (a, b) => a.or(b),
                },
                // Keep all shape samples within feather reach of the prior selection.
                "subtract" => prior().map(|area| window(area, feather as i32)),
                "intersect" => prior().zip(shape).and_then(|(a, b)| {
                    clip(overlap(a, b)).map(|area| overlap(window(area, feather as i32), b))
                }),
                _ => shape,
            }
        }
        .unwrap_or([0; 4]);
        let [left, top, right, bottom] = frame;
        let mut morphology = if op == "selection.modify" && left < right && top < bottom {
            Some(crate::selection::morphology::Rows::new(
                (right - left) as u32,
                (bottom - top) as u32,
                radius,
                expand,
                base.budget(),
            )?)
        } else {
            None
        };
        render(
            &base,
            session,
            frame,
            feather,
            &guarded,
            Some(&pinned),
            |y, row| {
                if let Some(morphology) = &mut morphology {
                    morphology.row(y, row, &pinned, |[l, t, r, b], values| {
                        values.fill(0);
                        let area = [l + frame[0], t + frame[1], r + frame[0], b + frame[1]];
                        let stride = (r - l) as usize;
                        let left = area[0].max(0);
                        let right = area[2].min(width as i32);
                        let top = area[1].max(0);
                        let bottom = area[3].min(height as i32);
                        if left >= right
                            || top >= bottom
                            || !selected.may_cover([left, top, right, bottom])?
                        {
                            return Ok(());
                        }
                        let rows =
                            crate::raster::backed::MAX_REGION_PIXELS / (right - left) as usize;
                        for top in (top..bottom).step_by(rows) {
                            let end = (top + rows as i32).min(bottom);
                            let view =
                                selected.prepare([left, top, right, end], &mut || guarded())?;
                            for yy in top..end {
                                pinned()?;
                                for x in left..right {
                                    values[(yy - area[1]) as usize * stride
                                        + (x - area[0]) as usize] =
                                        (view.value(x, yy)? * 255.).round() as u8;
                                }
                            }
                        }
                        Ok(())
                    })?;
                } else if op == "selection.invert" {
                    let view = selected
                        .prepare([0, y as i32, row.len() as i32, y as i32 + 1], &mut || {
                            guarded()
                        })?;
                    for (x, value) in row.iter_mut().enumerate() {
                        let before = if selected.bounds().is_some() {
                            view.value(x as i32, y as i32)?
                        } else {
                            0.
                        };
                        *value = (255. * (1. - before)).round() as u8;
                    }
                } else {
                    geometry.as_ref().unwrap().row_at(
                        frame[0],
                        frame[1] + y as i32,
                        row,
                        &mut crossings,
                    );
                }
                Ok(())
            },
            |y, row| {
                if mode == "replace" {
                    return Ok(());
                }
                let left = frame[0];
                let y = frame[1] + y as i32;
                let area = [left, y, left + row.len() as i32, y + 1];
                if selected.bounds().is_none() || !selected.may_cover(area)? {
                    if mode != "add" {
                        row.fill(0);
                    }
                    return Ok(());
                }
                let view = selected.prepare(area, &mut || guarded())?;
                for (x, value) in (left..).zip(row) {
                    let a = view.value(x, y)?;
                    let b = *value as f32 / 255.;
                    *value = (255.
                        * match mode {
                            "add" => a.max(b),
                            "subtract" => a * (1. - b),
                            "intersect" => a.min(b),
                            _ => unreachable!(),
                        })
                    .round() as u8;
                }
                Ok(())
            },
        )
        .map(Some)
    })();
    match prepared {
        Ok(current) => {
            let selection = batch.selection.as_mut().unwrap();
            selection.bounds = current.as_ref().map(|c| c.bounds);
            selection.current = current;
            apply_batch(edit, batch, session, &guarded)
        }
        Err(reason) => Err(cleanup(
            reason,
            batch.abort(session, &|| session.cleanup_check()),
        )),
    }
}
