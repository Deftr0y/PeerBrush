//! Source-backed internal pixels. Staging grants neither clipboard visibility nor
//! document publication; the nominal Engine receipt owns those final transitions.
use crate::{
    compositor::native::{Native16, Native8, Plan, RawLayers, Topology},
    engine::{
        native::{DocumentKey, NativeDocument, StagedOutput},
        scene::SceneSnapshot,
    },
    large_document::{Error, ScratchBytes, Store, TileSpec},
    raster::{backed::Region, Pixel16, Surface, TILE},
    retained::transaction::{self, Session},
    selection::NativeTileWriter,
    transform::native::{selected::Selection, Work},
};
use std::{cell::RefCell, sync::Arc};
#[path = "native/layers.rs"]
pub(crate) mod layers;
#[path = "native/promotion.rs"]
pub(crate) mod promotion;
#[cfg(test)]
#[path = "native/tests.rs"]
mod tests;

#[derive(Clone, Copy)]
pub(crate) struct Request<'a> {
    pub target: &'a str,
    pub mask: bool,
    pub merged: bool,
}

pub(crate) struct StagedCopy {
    source: DocumentKey,
    bounds: [i32; 4],
    output: StagedOutput,
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

/// Only the real unstyled regional provider is available at this boundary.
/// Unsupported derived work refuses before a private output is allocated.
pub(crate) fn stage_copy(
    document: &NativeDocument,
    scene: &SceneSnapshot,
    request: Request<'_>,
    session: &impl Session,
    work: &mut Work,
    check: &dyn Fn() -> Result<(), String>,
) -> Result<StagedCopy, String> {
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
    let metadata = scene.metadata();
    let area =
        metadata
            .selection()
            .unwrap_or([0, 0, metadata.width() as i32, metadata.height() as i32]);
    if area[0] >= area[2]
        || area[1] >= area[3]
        || area[2] <= 0
        || area[3] <= 0
        || area[0] >= metadata.width() as i32
        || area[1] >= metadata.height() as i32
    {
        return Err("Select an area inside the canvas to copy".into());
    }
    let area = [
        area[0].max(0),
        area[1].max(0),
        area[2].min(metadata.width() as i32),
        area[3].min(metadata.height() as i32),
    ];
    if metadata.depth() == 8 && !request.mask && metadata.profile().is_some() {
        return Err(
            "Clipboard color-profile conversion requires the qualified regional provider".into(),
        );
    }
    if request.merged
        && !request.mask
        && metadata
            .filters()
            .any(|effect| effect.enabled() && effect.weight() > 0.)
    {
        return Err("Clipboard global filters require the qualified regional provider".into());
    }
    let index = (!request.merged)
        .then(|| {
            metadata
                .layers()
                .position(|layer| layer.id() == request.target)
                .ok_or("Layer no longer exists")
        })
        .transpose()?;
    if !request.mask
        && index.is_some_and(|index| {
            metadata
                .layer(index)
                .is_some_and(|layer| layer.kind() == "adjustment")
        })
    {
        return Err("Clipboard adjustment pixels require the qualified regional provider".into());
    }
    let topology = Topology::for_target(scene, (!request.merged).then_some(request.target))?;
    let selection = metadata
        .selection()
        .map(|_| Selection::for_folder(document))
        .transpose()?;
    let failure = RefCell::new(None);
    let checked = || match session.check().and_then(|_| check()) {
        Ok(()) => Ok(()),
        Err(reason) => {
            *failure.borrow_mut() = Some(reason);
            Err(Error::Cancelled)
        }
    };
    // Prepare the first actual region before allocating any output fragment, so
    // unsupported masks/effects cannot create an accidental fallback candidate.
    let first = Region::new([
        area[0],
        area[1],
        (area[0] + TILE as i32).min(area[2]),
        (area[1] + TILE as i32).min(area[3]),
    ])
    .map_err(|e| e.to_string())?;
    match RawLayers::for_topology(&topology, first, &mut || checked()) {
        Ok(prepared) => drop(prepared),
        Err(reason) => return Err(failure.into_inner().unwrap_or(reason)),
    }
    if request.mask
        && index.is_some_and(|index| {
            metadata
                .layer(index)
                .is_some_and(|layer| layer.mask().is_some())
        })
    {
        return Err("Clipboard raw masks require the qualified regional provider".into());
    }
    let output = transaction::stage(document, session, check, |store, _| {
        write(
            scene,
            &topology,
            index,
            request,
            selection.as_ref(),
            area,
            store,
            work,
            &checked,
        )
    });
    let output = match output {
        Ok(output) => output,
        Err(reason) => return Err(failure.into_inner().unwrap_or(reason)),
    };
    Ok(StagedCopy {
        source: document.key(),
        bounds: area,
        output,
    })
}

#[allow(clippy::too_many_arguments)]
fn write(
    scene: &SceneSnapshot,
    topology: &Arc<Topology<'_>>,
    index: Option<usize>,
    request: Request<'_>,
    selection: Option<&Selection<'_>>,
    area: [i32; 4],
    store: &Store,
    work: &mut Work,
    check: &dyn Fn() -> crate::large_document::Result<()>,
) -> Result<Surface, String> {
    let (width, height) = ((area[2] - area[0]) as u32, (area[3] - area[1]) as u32);
    let depth = scene.metadata().depth();
    let spec = TileSpec::new(TILE as u16, TILE as u16, depth).map_err(|e| e.to_string())?;
    let mut writer =
        NativeTileWriter::new(width, height, depth, store.clone()).map_err(|e| e.to_string())?;
    let mut bytes =
        ScratchBytes::new(scene.budget(), spec.bytes(), spec.bytes()).map_err(|e| e.to_string())?;
    for ty in 0..height.div_ceil(TILE) {
        for tx in 0..width.div_ceil(TILE) {
            check().map_err(|e| e.to_string())?;
            bytes.fill(0);
            let rect = [
                area[0] + (tx * TILE) as i32,
                area[1] + (ty * TILE) as i32,
                (area[0] + (tx * TILE + TILE) as i32).min(area[2]),
                (area[1] + (ty * TILE + TILE) as i32).min(area[3]),
            ];
            let region = Region::new(rect).map_err(|e| e.to_string())?;
            let prepared = RawLayers::for_topology(topology, region, &mut || check())?;
            let selected = selection
                .map(|s| s.prepare(region, check))
                .transpose()
                .map_err(|e| e.to_string())?;
            if depth == 8 {
                let plan = Plan::<Native8, _>::with_topology(topology.clone(), &prepared, region)?;
                for y in rect[1]..rect[3] {
                    check().map_err(|e| e.to_string())?;
                    for x in rect[0]..rect[2] {
                        work.claim(1)?;
                        let mut pixel = if let Some(index) = index {
                            if request.mask {
                                let v = (plan.mask(index, x, y, true)? * 255.) as u8;
                                [v, v, v, 255]
                            } else {
                                plan.layer(index, x, y)?
                            }
                        } else {
                            plan.pixel(x, y)?
                        };
                        if !request.merged && !request.mask {
                            let index = index.ok_or("Missing clipboard layer")?;
                            let layer = scene
                                .metadata()
                                .layer(index)
                                .ok_or("Missing clipboard layer")?;
                            pixel[3] = (f32::from(pixel[3])
                                * plan.mask(index, x, y, false)?
                                * layer.opacity())
                            .round() as u8;
                        }
                        if let Some(selection) = &selected {
                            let factor = f32::from(selection.byte(x, y, work)?) / 255.;
                            pixel[3] = (f64::from(pixel[3]) * f64::from(factor)).round() as u8;
                            if pixel[3] == 0 {
                                pixel = [0; 4];
                            }
                        }
                        let at =
                            ((y - rect[1]) as usize * TILE as usize + (x - rect[0]) as usize) * 4;
                        bytes[at..at + 4].copy_from_slice(&pixel);
                    }
                }
            } else {
                let plan = Plan::<Native16, _>::with_topology(topology.clone(), &prepared, region)?;
                for y in rect[1]..rect[3] {
                    check().map_err(|e| e.to_string())?;
                    for x in rect[0]..rect[2] {
                        work.claim(1)?;
                        let mut pixel = if let Some(index) = index {
                            if request.mask {
                                let v = (plan.mask(index, x, y, true)? * 65535.).round() as u16;
                                [v, v, v, 65535]
                            } else {
                                plan.layer(index, x, y)?
                            }
                        } else {
                            plan.pixel(x, y)?
                        };
                        if !request.merged && !request.mask {
                            let index = index.ok_or("Missing clipboard layer")?;
                            let layer = scene
                                .metadata()
                                .layer(index)
                                .ok_or("Missing clipboard layer")?;
                            pixel[3] = (f64::from(pixel[3])
                                * plan.mask(index, x, y, false)?
                                * f64::from(layer.opacity()))
                            .round()
                            .clamp(0., 65535.) as u16;
                        }
                        if let Some(selection) = &selected {
                            let factor = f32::from(selection.byte(x, y, work)?) / 255.;
                            pixel[3] = (f64::from(pixel[3]) * f64::from(factor)).round() as u16;
                            if pixel[3] == 0 {
                                pixel = [0; 4];
                            }
                        }
                        let at =
                            ((y - rect[1]) as usize * TILE as usize + (x - rect[0]) as usize) * 8;
                        put16(&mut bytes[at..at + 8], pixel);
                    }
                }
            }
            drop(selected);
            drop(prepared);
            writer
                .tile(tx, ty, &bytes, &mut || check())
                .map_err(|e| e.to_string())?;
        }
    }
    writer.finish(&mut || check()).map_err(|e| e.to_string())
}
fn put16(out: &mut [u8], pixel: Pixel16) {
    for (pair, word) in out.chunks_exact_mut(2).zip(pixel) {
        pair.copy_from_slice(&word.to_be_bytes());
    }
}
