//! Source-backed internal pixels. Staging grants neither clipboard visibility nor
//! document publication; the nominal Engine receipt owns those final transitions.
use crate::{
    color_profile::prepared::{DisplayPlan, DisplayStatus},
    compositor::native::{
        prepare_filters_with_layers, LayerWindow, Native16, Native8, Plan, Stored, Topology,
    },
    edit_bounds::input::RasterInput,
    engine::{
        native::{DocumentKey, LayerMetadata, NativeDocument, StagedOutput},
        scene::SceneSnapshot,
    },
    large_document::{Error, ScratchBytes, Store},
    raster::{backed::Region, Pixel16, Surface, TILE},
    retained::transaction::{self, Session},
    selection::NativeRows,
    transform::native::{selected::Selection, Work},
    workspace::source_runtime::SourceSession,
};
use std::{cell::RefCell, sync::Arc};
#[path = "native/layers.rs"]
pub(crate) mod layers;
#[path = "native/promotion.rs"]
pub(crate) mod promotion;
mod transport;
pub(crate) use transport::CapturedImage;
pub(super) use transport::layout as transport_layout;

const SOURCE_COLUMNS: usize = crate::raster::NATIVE_EDGE.div_ceil(TILE) as usize + 1;

#[derive(Clone, Copy)]
pub(crate) enum PasteImage<'a> {
    Bitmap(&'a CapturedImage),
    Png(&'a Vec<u8>),
}

impl PasteImage<'_> {
    pub(crate) fn origin(self) -> Option<[i32; 2]> {
        match self {
            Self::Bitmap(image) => image.origin,
            Self::Png(_) => None,
        }
    }
}
#[cfg(test)]
#[path = "native/tests.rs"]
mod tests;

#[derive(Clone, Copy)]
pub(crate) struct Request<'a> {
    pub target: &'a str,
    pub mask: bool,
    pub merged: bool,
}

pub(crate) fn copy_area(document: &NativeDocument) -> Result<[i32; 4], String> {
    let metadata = document.metadata();
    let area = metadata.selection().unwrap_or([0, 0, metadata.width() as i32, metadata.height() as i32]);
    let area = super::clip_copy_area(area, metadata.width(), metadata.height())?;
    crate::raster::CanvasLimits::Native.check((area[2] - area[0]) as u32, (area[3] - area[1]) as u32)?;
    Ok(area)
}

pub(crate) struct StagedCopy {
    source: DocumentKey,
    bounds: [i32; 4],
    output: StagedOutput,
    budget: crate::large_document::ResidentBudget,
}
impl StagedCopy {
    pub(crate) fn source(&self) -> DocumentKey {
        self.source
    }
    pub(crate) fn origin(&self) -> [i32; 2] {
        [self.bounds[0], self.bounds[1]]
    }
    pub(crate) fn bounds(&self) -> [i32; 4] {
        self.bounds
    }
    pub(crate) fn into_parts(self) -> (DocumentKey, [i32; 4], StagedOutput) {
        (self.source, self.bounds, self.output)
    }
    pub(crate) fn abort(self, session: &impl Session) -> Result<(), String> {
        self.output.abort(session, &|| Ok(()))
    }
}

fn validate(
    document: &NativeDocument,
    scene: &SceneSnapshot,
    session: &impl Session,
    check: &dyn Fn() -> Result<(), String>,
) -> Result<(), String> {
    check()?;
    session.check()?;
    if scene.metadata().binding() != document.key()
        || !scene.budget().same_pool(document.budget())
        || !scene.disk_budget().same_pool(document.disk_budget())
    {
        return Err("Clipboard scene differs from the captured source generation".into());
    }
    if !document.budget().same_pool(&session.resident())
        || !document.disk_budget().same_pool(&session.disk())
    {
        return Err("Clipboard source session uses another admission".into());
    }
    if scene.metadata().read_only() {
        return Err("Protected clipboard pixels require their preserved composite provider".into());
    }
    Ok(())
}

/// Copy the same raw mask plane resolved by paint.clear_selection. A derived
/// mask can include other paint steps, fills and spatial effects.
pub(super) fn stage_mask_cut(
    document: &NativeDocument,
    scene: &SceneSnapshot,
    command: &serde_json::Value,
    session: &impl Session,
    work: &mut Work,
    check: &dyn Fn() -> Result<(), String>,
) -> Result<StagedCopy, String> {
    validate(document, scene, session, check)?;
    if document.metadata().selection().is_none() {
        return Err("Select pixels before cutting".into());
    }
    let input = RasterInput::from_parameters(document, command)?;
    if !input.is_mask() {
        return Err("Mask cut requires a raw paint step".into());
    }
    let area = copy_area(document)?;
    let [left, top, right, bottom] = area;
    let [ox, oy] = input.origin();
    let (width, height) = ((right - left) as u32, (bottom - top) as u32);
    let selection = Selection::for_folder(document)?;
    let checked = || session.check().and_then(|_| check());
    let pinned = || check().and_then(|_| session.check_pinned_work());
    let output = transaction::stage(document, session, &checked, |store, guard| {
        let mut writer = NativeRows::new(width, height, input.depth(), store.clone())
            .map_err(|e| e.to_string())?;
        let row_bytes = width as usize * 8;
        let mut row = ScratchBytes::new(document.budget(), row_bytes, row_bytes)
            .map_err(|e| e.to_string())?;
        let band = (crate::raster::backed::MAX_REGION_PIXELS / width as usize).clamp(1, TILE as usize) as i32;
        let mut y = top;
        while y < bottom {
            checked()?;
            let end = (y + band).min(bottom);
            let mut source_spans = [[0; 2]; SOURCE_COLUMNS];
            let count = physical_spans(
                input.surface(), [ox, oy], [left, y, right, end], &mut source_spans,
            );
            let mut covered = [[0; 2]; 2 * SOURCE_COLUMNS];
            let spans = coverage_spans(&selection, &source_spans[..count], [y, end], &mut covered)?;
            let (Some(first), Some(last)) = (spans.first(), spans.last()) else {
                writer.zero_rows((end - y) as u32, &mut || guard()).map_err(|e| e.to_string())?;
                y = end;
                continue;
            };
            let region = Region::new([first[0], y, last[1], end]).map_err(|e| e.to_string())?;
            let stencil = selection.prepare(region, &|| guard()).map_err(|e| e.to_string())?;
            let raw = input.prepare_region(
                [first[0] - ox, y - oy, last[1] - ox, end - oy], &mut || checked(),
            )?;
            for py in y..end {
                pinned()?;
                for x in spans.iter().flat_map(|&[left, right]| left..right) {
                    work.claim(1)?;
                    let factor = f32::from(stencil.byte(x, py, work)?) / 255.;
                    let pixel = raw.pixel16(x - ox, py - oy)?;
                    let at = (x - left) as usize * 8;
                    let out = &mut row[at..at + 8];
                    put16(out, super::mask_cut_sample(pixel, factor, input.depth()));
                }
                let local = spans.iter().map(|&[start, end]| (start - left) as usize..(end - left) as usize);
                let row_y = (py - top) as u32;
                let flushes = writer.row_flushes(row_y);
                writer.row_spans(row_y, &row, 8, local, &mut || {
                    if flushes { guard() } else { pinned().map_err(|_| Error::Cancelled) }
                })
                    .map_err(|e| e.to_string())?;
            }
            y = end;
        }
        writer.finish(&mut || guard()).map_err(|e| e.to_string())
    })?;
    Ok(StagedCopy { source: document.key(), bounds: area, output, budget: document.budget().clone() })
}

fn physical_spans(
    source: &Surface,
    [x, y]: [i32; 2],
    [mut left, top, end, bottom]: [i32; 4],
    spans: &mut [[i32; 2]; SOURCE_COLUMNS],
) -> usize {
    let mut count = 0;
    while left < end {
        let right = end.min(((left - x).div_euclid(TILE as i32) + 1) * TILE as i32 + x);
        if source.has_physical_tiles([left - x, top - y, right - x, bottom - y]) {
            if count > 0 && spans[count - 1][1] == left {
                spans[count - 1][1] = right;
            } else {
                spans[count] = [left, right];
                count += 1;
            }
        }
        left = right;
    }
    count
}

fn coverage_spans<'a>(
    selection: &Selection<'_>,
    spans: &[[i32; 2]],
    [top, bottom]: [i32; 2],
    covered: &'a mut [[i32; 2]; 2 * SOURCE_COLUMNS],
) -> Result<&'a [[i32; 2]], String> {
    let origin = selection.origin()[0];
    let mut used = 0;
    for &[mut left, end] in spans {
        while left < end {
            let edge = ((left - origin).div_euclid(TILE as i32) + 1) * TILE as i32 + origin;
            let right = end.min(edge);
            if selection.may_cover([left, top, right, bottom]) {
                if used > 0 && covered[used - 1][1] == left {
                    covered[used - 1][1] = right;
                } else {
                    *covered.get_mut(used)
                        .ok_or("Clipboard coverage spans exceed admission")? = [left, right];
                    used += 1;
                }
            }
            left = right;
        }
    }
    Ok(&covered[..used])
}

/// Derived styles retire before the clipboard result can leave preparation.
pub(crate) fn stage_copy(
    document: &NativeDocument,
    scene: &SceneSnapshot,
    request: Request<'_>,
    session: &SourceSession,
    work: &mut Work,
    check: &dyn Fn() -> Result<(), String>,
) -> Result<StagedCopy, String> {
    stage_pixels(document, scene, request, session, work, check, true)
}

pub(crate) fn stage_copy_in_document(
    document: &NativeDocument,
    scene: &SceneSnapshot,
    request: Request<'_>,
    session: &SourceSession,
    work: &mut Work,
    check: &dyn Fn() -> Result<(), String>,
) -> Result<StagedCopy, String> {
    stage_pixels(document, scene, request, session, work, check, false)
}

fn stage_pixels(
    document: &NativeDocument,
    scene: &SceneSnapshot,
    request: Request<'_>,
    session: &SourceSession,
    work: &mut Work,
    check: &dyn Fn() -> Result<(), String>,
    display_color: bool,
) -> Result<StagedCopy, String> {
    validate(document, scene, session, check)?;
    let checked = || check().and_then(|_| session.check());
    let index = copy_target(scene, request)?;
    let topology = Topology::for_preview(scene, index)?;
    let area = copy_area(document)?;
    let crop = request.merged.then_some(area);
    let stored = if request.mask && index.is_some() {
        None
    } else if crop.is_some() && Stored::composite_needed(&topology, crop) {
        Some(Stored::prepare_composite(topology.clone(), session, crop, &mut || checked())?)
    } else if Stored::needed(&topology) {
        Some(Stored::prepare_cropped(topology.clone(), session, area, &mut || checked())?)
    } else {
        None
    };
    let result = stage_with_layers(
        document,
        &topology,
        stored.as_ref(),
        index,
        request.mask,
        session,
        work,
        check,
        display_color,
    );
    let cleanup = stored.map_or(Ok(()), |stored| {
        stored.retire(&mut || session.cleanup_check())
    });
    match (result, cleanup) {
        (result, Ok(())) => result.and_then(|copy| match session.check().and_then(|_| check()) {
            Ok(()) => Ok(copy),
            Err(reason) => match copy.output.abort(session, &|| session.cleanup_check()) {
                Ok(()) => Err(reason),
                Err(cleanup) => Err(format!("{reason}; clipboard output retirement: {cleanup}")),
            },
        }),
        (Err(reason), Err(cleanup)) => {
            Err(format!("{reason}; clipboard style retirement: {cleanup}"))
        }
        (Ok(copy), Err(reason)) => match copy.output.abort(session, &|| session.cleanup_check()) {
            Ok(()) => Err(reason),
            Err(cleanup) => Err(format!("{reason}; clipboard output retirement: {cleanup}")),
        },
    }
}

fn copy_target(scene: &SceneSnapshot, request: Request<'_>) -> Result<Option<usize>, String> {
    if request.merged {
        return Ok(None);
    }
    scene.metadata().layers()
        .position(|layer| layer.id() == request.target)
        .map(Some)
        .ok_or_else(|| "Layer no longer exists".into())
}

#[cfg(test)]
fn stage_regional_copy(
    document: &NativeDocument,
    scene: &SceneSnapshot,
    request: Request<'_>,
    session: &impl Session,
    work: &mut Work,
    check: &dyn Fn() -> Result<(), String>,
) -> Result<StagedCopy, String> {
    validate(document, scene, session, check)?;
    let index = copy_target(scene, request)?;
    stage_with_layers(
        document,
        &Topology::for_preview(scene, index)?,
        None,
        index,
        request.mask,
        session,
        work,
        check,
        true,
    )
}

#[allow(clippy::too_many_arguments)]
fn stage_with_layers<'a>(
    document: &NativeDocument,
    topology: &Arc<Topology<'a>>,
    stored: Option<&Stored<'a>>,
    index: Option<usize>,
    mask: bool,
    session: &impl Session,
    work: &mut Work,
    check: &dyn Fn() -> Result<(), String>,
    display_color: bool,
) -> Result<StagedCopy, String> {
    let scene = topology.scene();
    let metadata = scene.metadata();
    let area = copy_area(document)?;
    let mask_layer = index.filter(|_| mask)
        .map(|i| metadata.layer(i).ok_or("Missing clipboard layer"))
        .transpose()?;
    let selection = metadata
        .selection()
        .map(|_| Selection::for_folder(document))
        .transpose()?;
    let failure = RefCell::new(None);
    let checked = || {
        session.check().and_then(|_| check()).map_err(|reason| {
            *failure.borrow_mut() = Some(reason.clone());
            reason
        })
    };
    let pinned = || check().and_then(|_| session.check_pinned_work()).map_err(|reason| {
        *failure.borrow_mut() = Some(reason.clone());
        reason
    });
    let display = if metadata.depth() == 8 && display_color {
        Some(DisplayPlan::prepare(
            metadata.profile(),
            mask_layer.is_some(),
            scene.budget(),
            &mut || checked(),
        )?)
    } else {
        None
    };
    let first = Region::new([area[0], area[1], area[2], area[1] + 1]).map_err(|e| e.to_string())?;
    // Check actual source/provider readability before creating an output.
    if let Some(layer) = mask_layer {
        if layer.mask().is_some() {
            drop(crate::mask::region::prepare_scene(scene, layer, first.bounds(), &mut || checked())?);
        }
    } else {
        drop(LayerWindow::prepare(topology, first, stored, &mut || checked())?);
    }
    if index.is_none() {
        drop(prepare_filters_with_layers(
            topology,
            first,
            stored,
            &mut || checked(),
        )?);
    }
    let output = transaction::stage(document, session, &checked, |store, _| {
        write(
            topology,
            stored,
            index,
            mask_layer,
            selection.as_ref(),
            display.as_ref(),
            area,
            store,
            work,
            &checked,
            &pinned,
        )
    })
    .map_err(|reason| {
        failure.into_inner().map_or(reason.clone(), |cause| {
            reason.replacen(&Error::Cancelled.to_string(), &cause, 1)
        })
    })?;
    Ok(StagedCopy {
        source: document.key(),
        bounds: area,
        output,
        budget: document.budget().clone(),
    })
}

#[allow(clippy::too_many_arguments)]
fn write<'a>(
    topology: &Arc<Topology<'a>>,
    stored: Option<&Stored<'a>>,
    index: Option<usize>,
    mask_layer: Option<LayerMetadata<'a>>,
    selection: Option<&Selection<'_>>,
    display: Option<&DisplayPlan>,
    area: [i32; 4],
    store: &Store,
    work: &mut Work,
    check: &dyn Fn() -> Result<(), String>,
    pinned: &dyn Fn() -> Result<(), String>,
) -> Result<Surface, String> {
    let scene = topology.scene();
    let (width, height) = ((area[2] - area[0]) as u32, (area[3] - area[1]) as u32);
    let depth = scene.metadata().depth();
    let mask = mask_layer.is_some();
    let mask_origin = mask_layer.map_or([0, 0], |layer| layer.origin());
    let cms = display.filter(|plan| plan.status() == DisplayStatus::Converted);
    // Selection clears hidden RGB at zero alpha, even after native8 display
    // conversion. Masks/opacity cannot reveal absent paint; effects and other
    // layer kinds may generate pixels and must keep the full rendering path.
    let sparse = index
        .filter(|_| selection.is_some() && !mask)
        .and_then(|i| scene.metadata().layer(i))
        .filter(|layer| layer.kind() == "paint" && !layer.effects().any(|e| e.enabled() && e.weight() > 0.))
        .map(|layer| scene.raster(layer.pixels().slot()).map(|pixels| (pixels, layer.origin())))
        .transpose()?;
    let merged_tiles = if index.is_none() && selection.is_some() {
        topology.composite_tiles(Some(area), &mut || check())?
    } else {
        None
    };
    let mut writer =
        NativeRows::new(width, height, depth, store.clone()).map_err(|e| e.to_string())?;
    let row_bytes = width as usize * usize::from(depth / 8) * 4;
    let mut row = ScratchBytes::new(scene.budget(), row_bytes, row_bytes)
        .map_err(|e| e.to_string())?;
    let mut cms_row = cms.map(|_| ScratchBytes::new(scene.budget(), row_bytes, row_bytes))
        .transpose().map_err(|e| e.to_string())?;
    let band = (crate::raster::backed::MAX_REGION_PIXELS / width as usize)
        .min(TILE as usize)
        .max(1) as i32;
    let mut top = area[1];
    while top < area[3] {
        check()?;
        let bottom = (top + band).min(area[3]);
        // A native-width interval intersects at most this many source columns,
        // including a partial tile at either end. Adjacent occupied spans merge.
        let mut spans = [[0; 2]; SOURCE_COLUMNS];
        let mut count = 0;
        if let Some((source, origin)) = sparse {
            count = physical_spans(source, origin, [area[0], top, area[2], bottom], &mut spans);
        } else if let Some(tiles) = &merged_tiles {
            for span in tiles.column_spans([area[0], top, area[2], bottom]) {
                spans[count] = span;
                count += 1;
            }
        } else {
            spans[0] = [area[0], area[2]];
            count = 1;
        }
        let mut covered = [[0; 2]; 2 * SOURCE_COLUMNS];
        let spans = if let Some(selection) = selection {
            coverage_spans(selection, &spans[..count], [top, bottom], &mut covered)?
        } else {
            &spans[..count]
        };
        let (Some(first), Some(last)) = (spans.first(), spans.last()) else {
            writer.zero_rows((bottom - top) as u32, &mut || check().map_err(|_| Error::Cancelled))
                .map_err(|e| e.to_string())?;
            top = bottom;
            continue;
        };
        let sparse_rows = spans != [[area[0], area[2]]];
        let columns = || spans.iter().flat_map(|&[left, right]| left..right);
        let local = || spans.iter().map(|&[left, right]| (left - area[0]) as usize..(right - area[0]) as usize);
        let region = Region::new([first[0], top, last[1], bottom]).map_err(|e| e.to_string())?;
        let filtered = if index.is_none() {
            prepare_filters_with_layers(topology, region, stored, &mut || check())?
        } else {
            None
        };
        let raw_mask = mask_layer.filter(|layer| layer.mask().is_some())
            .map(|layer| crate::mask::region::prepare_scene(scene, layer, region.bounds(), &mut || check()))
            .transpose()?;
        let prepared = if filtered.is_none() && !mask {
            Some(LayerWindow::prepare(topology, region, stored, &mut || {
                check()
            })?)
        } else {
            None
        };
        let selected = selection
            .map(|s| s.prepare(region, &|| check().map_err(|_| Error::Cancelled)))
            .transpose()
            .map_err(|e| e.to_string())?;
        if depth == 8 {
            let plan = prepared
                .as_ref()
                .map(|p| Plan::<Native8, _>::with_topology(topology.clone(), p, region))
                .transpose()?;
            for y in top..bottom {
                pinned()?;
                if sparse_rows && cms.is_some() { row.fill(0); }
                for x in columns() {
                    let at = (x - area[0]) as usize * 4;
                    let pixel = &mut row[at..at + 4];
                    work.claim(1)?;
                    let value = if mask {
                        let gray = match &raw_mask {
                            Some(mask) => (mask.value8(x - mask_origin[0], y - mask_origin[1])? * 255.) as u8,
                            None => 255,
                        };
                        [gray, gray, gray, 255]
                    } else if let Some(image) = &filtered {
                        image.pixel(x, y)?
                    } else {
                        let p = plan.as_ref().ok_or("Missing clipboard provider")?;
                        if let Some(i) = index {
                            p.layer(i, x, y)?
                        } else {
                            p.pixel(x, y)?
                        }
                    };
                    pixel.copy_from_slice(&value);
                }
                let output = if let Some(cms) = cms {
                    let converted = cms_row.as_mut().ok_or("Missing clipboard color conversion row")?;
                    cms.convert8(&row, converted, &mut || pinned())?;
                    converted
                } else {
                    &mut row
                };
                for x in columns() {
                    let at = (x - area[0]) as usize * 4;
                    let pixel = &mut output[at..at + 4];
                    if let Some(i) = index.filter(|_| !mask) {
                        let p = plan.as_ref().ok_or("Missing clipboard mask provider")?;
                        pixel[3] = (f32::from(pixel[3])
                            * p.mask(i, x, y, false)?
                            * scene
                                .metadata()
                                .layer(i)
                                .ok_or("Missing clipboard layer")?
                                .opacity())
                        .round() as u8;
                    }
                    if let Some(selected) = &selected {
                        let factor = f32::from(selected.byte(x, y, work)?) / 255.;
                        pixel[3] = (f64::from(pixel[3]) * f64::from(factor)).round() as u8;
                        if pixel[3] == 0 {
                            pixel.fill(0);
                        }
                    }
                }
                let row_y = (y - area[1]) as u32;
                let guard = if writer.row_flushes(row_y) { check } else { pinned };
                writer.row_spans(row_y, output, 4, local(), &mut || guard().map_err(|_| Error::Cancelled))
                    .map_err(|e| e.to_string())?;
            }
        } else {
            let plan = prepared
                .as_ref()
                .map(|p| Plan::<Native16, _>::with_topology(topology.clone(), p, region))
                .transpose()?;
            for y in top..bottom {
                pinned()?;
                for x in columns() {
                    let at = (x - area[0]) as usize * 8;
                    let output = &mut row[at..at + 8];
                    work.claim(1)?;
                    let mut pixel = if mask {
                        let gray = match &raw_mask {
                            Some(mask) => (mask.value16(x - mask_origin[0], y - mask_origin[1])? * 65535.).round() as u16,
                            None => 65535,
                        };
                        [gray, gray, gray, 65535]
                    } else if let Some(image) = &filtered {
                        image.pixel16(x, y)?
                    } else {
                        let p = plan.as_ref().ok_or("Missing clipboard provider")?;
                        if let Some(i) = index {
                            p.layer(i, x, y)?
                        } else {
                            p.pixel(x, y)?
                        }
                    };
                    if let Some(i) = index.filter(|_| !mask) {
                        let p = plan.as_ref().ok_or("Missing clipboard mask provider")?;
                        pixel[3] = (f64::from(pixel[3])
                            * p.mask(i, x, y, false)?
                            * f64::from(
                                scene
                                    .metadata()
                                    .layer(i)
                                    .ok_or("Missing clipboard layer")?
                                    .opacity(),
                            ))
                        .round()
                        .clamp(0., 65535.) as u16;
                    }
                    if let Some(selected) = &selected {
                        let factor = f32::from(selected.byte(x, y, work)?) / 255.;
                        pixel[3] = (f64::from(pixel[3]) * f64::from(factor)).round() as u16;
                        if pixel[3] == 0 {
                            pixel = [0; 4];
                        }
                    }
                    put16(output, pixel);
                }
                let row_y = (y - area[1]) as u32;
                let guard = if writer.row_flushes(row_y) { check } else { pinned };
                writer.row_spans(row_y, &row, 8, local(), &mut || guard().map_err(|_| Error::Cancelled))
                    .map_err(|e| e.to_string())?;
            }
        }
        top = bottom;
    }
    writer
        .finish(&mut || check().map_err(|_| Error::Cancelled))
        .map_err(|e| e.to_string())
}
fn put16(out: &mut [u8], pixel: Pixel16) {
    for (pair, word) in out.chunks_exact_mut(2).zip(pixel) {
        pair.copy_from_slice(&word.to_be_bytes());
    }
}
