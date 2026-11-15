//! Whole-document filters consume bounded composite stripes, including their
//! finite-support halo. Only the final derived output survives preparation.
use super::*;
use crate::{effects::{regional, stored, Effect}, workspace::source_runtime::SourceSession};
use std::{cell::RefCell, sync::Arc};

pub(crate) fn prepare(
    topology: &Arc<Topology<'_>>,
    region: Region,
    check: &mut dyn FnMut() -> Result<()>,
) -> Result<Option<regional::RegionalImage>> {
    prepare_with_layers(topology, region, None, check)
}
pub(crate) fn prepare_with_layers<'a>(
    topology: &Arc<Topology<'a>>,
    region: Region,
    stored: Option<&Stored<'a>>,
    check: &mut dyn FnMut() -> Result<()>,
) -> Result<Option<regional::RegionalImage>> {
    prepare_with_checks(topology, region, stored, &mut ReadChecks::caller_only(check))
}
pub(crate) fn prepare_with_checks<'a>(
    topology: &Arc<Topology<'a>>,
    region: Region,
    stored: Option<&Stored<'a>>,
    checks: &mut ReadChecks<'_>,
) -> Result<Option<regional::RegionalImage>> {
    checks.full()?;
    let scene = topology.scene;
    let doc = scene.metadata();
    if topology.target().is_some() || !doc.filters().any(|f| f.enabled() && f.weight() > 0.) {
        return Ok(None);
    }
    if let Some(stored) = stored {
        stored.check_tree(topology)?;
        if let Some(image) = stored.filtered(region, &mut || checks.full())? {
            return Ok(Some(image));
        }
    }
    let (_metadata, filters) = capture_filters(scene, &mut || checks.full())?;
    let no_bitmaps = vec![None; filters.len()];
    let plan = regional::Plan::new_prepared(
        regional::Frame::new(doc.width(), doc.height())?,
        region,
        doc.depth(),
        &filters,
        scene.budget(),
        &no_bitmaps,
    )?;
    let [left, top, right, bottom] = plan.input_bounds();
    let band_rows = (crate::raster::backed::MAX_REGION_PIXELS / (right - left) as usize) as i32;
    let checks = RefCell::new(checks);
    let mut current: Option<regional::RegionalImage> = None;
    let output = plan.render(
        scene.budget(),
        &vec![None; filters.len()],
        &vec![None; filters.len()],
        &mut || checks.borrow_mut().pinned(),
        |x, y| {
            if current.as_ref().is_none_or(|image| {
                let b = image.bounds();
                y < b[1] || y >= b[3]
            }) {
                drop(current.take());
                let start = top + (y - top) / band_rows * band_rows;
                let band = Region::new([left, start, right, (start + band_rows).min(bottom)])
                    .map_err(|e| e.to_string())?;
                current = Some(composite(topology, band, stored, &mut **checks.borrow_mut())?);
            }
            current.as_ref().ok_or("Missing prepared filter input")?.pixel16(x, y)
        },
    )?;
    checks.into_inner().full()?;
    Ok(Some(output))
}

pub(super) fn needs_stage(topology: &Topology<'_>, crop: Option<[i32; 4]>) -> bool {
    if topology.target().is_some() {
        return false;
    }
    let doc = topology.scene.metadata();
    let halo: u64 = doc.filters()
        .filter(|filter| {
            filter.enabled() && filter.weight() > 0. && matches!(filter.kind(), "blur" | "bloom")
        })
        .filter_map(|filter| {
            crate::effects::validate(filter.kind(), filter.settings()).ok()?;
            regional::reach(filter.kind(), filter.settings()).ok()
        })
        .map(|radius| radius as u64)
        .sum();
    if halo == 0 {
        return false;
    }
    let Ok(frame) = regional::Frame::new(doc.width(), doc.height()) else { return false };
    let crop = crop.unwrap_or([0, 0, doc.width() as i32, doc.height() as i32]);
    if crop[0] >= crop[2] || crop[1] >= crop[3] || crop != frame.expand(crop, 0) {
        return false;
    }
    let [width, height] = [(crop[2] - crop[0]) as u64, (crop[3] - crop[1]) as u64];
    // Upper bound for any admitted output rectangle expanded in both axes.
    let pixels = (crate::raster::backed::MAX_REGION_PIXELS as u64).min(width * height);
    let longest = width.max(height);
    let axes = (longest + pixels / longest).min(width + height);
    let [l, t, r, b] = frame.expand(crop, halo as i32);
    let expanded = (pixels + 2 * halo * axes + 4 * halo * halo).min((r - l) as u64 * (b - t) as u64);
    let bytes = expanded * if doc.depth() == 16 { 32 } else { 16 } + 512 * 1024;
    bytes > (topology.scene.budget().limit() as u64 / 4).min(64 * 1024 * 1024)
}

pub(super) fn stage<'a>(
    topology: &Arc<Topology<'a>>,
    layers: &Stored<'a>,
    session: &'a SourceSession,
    bounds: [i32; 4],
    check: &mut dyn FnMut() -> Result<()>,
) -> Result<stored::Image<'a>> {
    let scene = topology.scene;
    let doc = scene.metadata();
    let (_metadata, filters) = capture_filters(scene, check)?;
    let size = [doc.width(), doc.height()];
    stored::filter_stack(
        session, scene.render_key()?, size, bounds, doc.depth(), &filters,
        check, |region, check| composite(topology, region, Some(layers), &mut ReadChecks::caller_only(check)),
    )
}

fn capture_filters(scene: &SceneSnapshot, check: &mut dyn FnMut() -> Result<()>) -> Result<(Reservation, Vec<Effect>)> {
    let doc = scene.metadata();
    let mut bytes = 65536usize;
    for filter in doc.filters() {
        check()?;
        if filter.clips().len() != 0 {
            return Err("Document filters cannot contain layer selection clips".into());
        }
        bytes = bytes
            .checked_add(filter.id().len() + filter.kind().len())
            .ok_or("Filter metadata admission overflow")?;
        regional::metadata_bytes(filter.settings(), 0, &mut bytes)?;
    }
    let metadata = scene
        .budget()
        .reserve_bytes(bytes, bytes)
        .map_err(|e| e.to_string())?;
    let mut filters = Vec::new();
    filters
        .try_reserve_exact(doc.filters().len())
        .map_err(|_| "Filter metadata allocation refused")?;
    for filter in doc.filters() {
        filters.push(Effect {
            id: filter.id().into(),
            kind: filter.kind().into(),
            enabled: filter.enabled(),
            weight: filter.weight(),
            settings: Arc::new(filter.settings().clone()),
        });
    }
    crate::filters::validate_regional(&filters)?;
    Ok((metadata, filters))
}

fn composite<'a>(
    topology: &Arc<Topology<'a>>,
    band: Region,
    stored: Option<&Stored<'a>>,
    checks: &mut ReadChecks<'_>,
) -> Result<regional::RegionalImage> {
    let scene = topology.scene;
    let depth = scene.metadata().depth();
    let layers = LayerWindow::prepare_with_checks(topology, band, stored, checks)?;
    let pixels = if depth == 8 {
        let plan = Plan::<Native8, _>::with_topology(topology.clone(), &layers, band)?;
        crate::render::region::native(band, depth, scene.budget(), &mut || checks.pinned(), |x, y| {
            Ok(plan.pixel(x, y)?.map(|v| u16::from(v) * 257))
        })?
    } else {
        let plan = Plan::<Native16, _>::with_topology(topology.clone(), &layers, band)?;
        crate::render::region::native(band, depth, scene.budget(), &mut || checks.pinned(), |x, y| plan.pixel(x, y))?
    };
    checks.full()?;
    regional::RegionalImage::from_native(band, depth, pixels)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn finite_filter_halos_cross_composite_bands_without_losing_native_samples() {
        for depth in [8, 16] {
            let mut doc = crate::engine::Document::new_depth(769, 521, depth).unwrap();
            for y in 0..521 {
                for x in 0..769 {
                    doc.layers[0].pixels.set16(
                        x,
                        y,
                        [
                            ((x * 941 + y * 31 + 12347) % 65536) as u16,
                            ((y * 617 + x * 43 + 23521) % 65536) as u16,
                            43217,
                            if (x + y) % 7 == 0 { 0 } else { 32769 },
                        ],
                    );
                }
            }
            doc.layers[0].effects.push(Effect {
                id: crate::engine::id(),
                kind: "invert".into(),
                enabled: true,
                weight: 0.43,
                settings: serde_json::json!({}).into(),
            });
            doc.layers[0].effects.push(Effect {
                id: crate::engine::id(),
                kind: "blur".into(),
                enabled: true,
                weight: 0.57,
                settings: serde_json::json!({"radius":3.1}).into(),
            });
            doc.layers[0].x = -3;
            doc.layers[0].y = 2;
            doc.layers[0].effects.push(Effect {
                id: crate::engine::id(),
                kind: "stroke".into(),
                enabled: true,
                weight: 0.67,
                settings: serde_json::json!({"size":3.7}).into(),
            });
            doc.layers[0].effects.push(Effect {
                id: crate::engine::id(),
                kind: "blur".into(),
                enabled: true,
                weight: 0.73,
                settings: serde_json::json!({"radius":1.7}).into(),
            });
            let paint_id = doc.layers[0].id.clone();
            let mut folder = crate::engine::Layer::new("Spatial folder", "group", 769, 521);
            folder.pixels = crate::raster::Raster::new_depth(769, 521, depth);
            folder.opacity = 0.67;
            folder.effects.push(Effect {
                id: crate::engine::id(),
                kind: "blur".into(),
                enabled: true,
                weight: 0.73,
                settings: serde_json::json!({"radius":2.7}).into(),
            });
            doc.layers[0].parent = Some(folder.id.clone());
            doc.layers.insert(0, folder);
            crate::adjustment::add(&mut doc, &serde_json::json!({"kind":"invert"})).unwrap();
            doc.layers[0].effects.push(Effect {
                id: crate::engine::id(),
                kind: "bloom".into(),
                enabled: true,
                weight: 0.61,
                settings: serde_json::json!({"spread":3.1}).into(),
            });
            for (kind, settings) in [
                ("blur", serde_json::json!({"radius":2.3})),
                ("bloom", serde_json::json!({"spread":3.7})),
                (
                    "channel_clamp",
                    serde_json::json!({"channel":"r","minimum":0.13,"maximum":0.83}),
                ),
            ] {
                doc.filters.push(Effect {
                    id: crate::engine::id(),
                    kind: kind.into(),
                    enabled: true,
                    weight: 0.67,
                    settings: settings.into(),
                });
            }
            let (scene, path) = super::super::tests::capture_kind_with_budget(
                &doc,
                true,
                ResidentBudget::new(96 * 1024 * 1024).unwrap(),
            );
            let topology = Topology::new(&scene).unwrap();
            let regions = [[0, 0, 200, 100], [3, 173, 760, 345], [590, 400, 769, 521]];
            for bounds in regions {
                let region = Region::new(bounds).unwrap();
                let output = prepare(&topology, region, &mut || Ok(())).unwrap().unwrap();
                assert!(output.uses_budget(scene.budget()));
                if depth == 8 {
                    let expected = crate::filters::prepare8(&doc).unwrap();
                    for y in bounds[1]..bounds[3] {
                        for x in bounds[0]..bounds[2] {
                            assert_eq!(
                                output.pixel(x, y).unwrap(),
                                expected.get(x, y),
                                "8 {x},{y}"
                            );
                        }
                    }
                } else {
                    let expected = crate::filters::prepare16(&doc).unwrap();
                    for y in bounds[1]..bounds[3] {
                        for x in bounds[0]..bounds[2] {
                            assert_eq!(
                                output.pixel16(x, y).unwrap(),
                                expected.get(x, y),
                                "16 {x},{y}"
                            );
                        }
                    }
                }
            }
            let baseline = scene.budget().used();
            let mut calls = 0;
            let region = Region::new(regions[1]).unwrap();
            let failed = prepare(&topology, region, &mut || {
                calls += 1;
                if calls > 30 {
                    Err("cancel filter input".into())
                } else {
                    Ok(())
                }
            });
            assert_eq!(failed.err().unwrap(), "cancel filter input");
            assert_eq!(scene.budget().used(), baseline);
            let free = scene.budget().limit() - baseline;
            let held = scene.budget().reserve_bytes(free, free).unwrap();
            assert!(prepare(&topology, region, &mut || Ok(())).is_err());
            assert_eq!(scene.budget().used(), scene.budget().limit());
            drop(held);
            let isolated = Topology::for_target(&scene, Some(&paint_id)).unwrap();
            assert!(prepare(&isolated, region, &mut || Ok(()))
                .unwrap()
                .is_none());
            drop(isolated);
            drop(topology);
            drop(scene);
            super::super::tests::cleanup(&path);
        }
    }
}
