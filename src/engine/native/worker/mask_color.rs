//! Color masks sample rendered layer colors, not the display or current selection.
use super::*;
use crate::{
    compositor::native::{LayerWindow, Native16, Native8, Plan, Stored, Topology},
    engine::scene::{SceneIdentity, SceneSnapshot},
    large_document::{Error, ScratchBytes},
    raster::{
        backed::{Region, MAX_REGION_PIXELS},
        surface::{Surface, SurfaceRegion},
    },
    selection::{
        backed::CoverageSource,
        regional_masks::{self, Prepared, Source},
        worker::{self, Combine},
    },
    smart_mask::Parameters,
};
use std::cell::RefCell;
#[cfg(test)]
#[path = "mask_color_tests.rs"]
mod tests;

pub(super) fn prepare(
    edit: CapturedEdit,
    session: &SourceSession,
    check: &dyn Fn() -> Result<()>,
) -> Result<PreparedDelta> {
    let base = edit.document().clone();
    let cancellation = edit.cancellation();
    let checked = || {
        cancellation.check()?;
        session.check()?;
        check()
    };
    checked()?;
    let command = edit
        .descriptor()
        .ok_or("Color mask descriptor missing")?
        .commands()
        .next()
        .ok_or("Color mask command missing")?;
    if command.method() != Method::Mask || command.parameters()["op"] != "mask.from_color" {
        return Err("Color mask differs from its captured command".into());
    }
    let token = command.token();
    let layer = super::super::insertion::mask_target(&base, command.parameters())?;
    let parameters = Parameters::new(
        command.parameters(),
        layer.origin(),
        [layer.pixels().width(), layer.pixels().height()],
    )?;
    let guide = colors(&base, layer, session, &checked)?;
    let mut sampled_origin = [0; 2];
    let sampled = crate::retained::transaction::stage(&base, session, &checked, |store, guard| {
        let selected = sample(guide.surface(), &parameters, store.clone(), guard)?;
        sampled_origin = selected.origin();
        Ok(selected.mask().clone())
    });
    let sampled = super::mask::finish(sampled, Some(guide), session)?;
    let result = crate::retained::transaction::stage(&base, session, &checked, |store, guard| {
        let mut source = regional_masks::native::Native::new(&base, layer, &checked)?;
        let (w, h, depth) = (
            layer.pixels().width(),
            layer.pixels().height(),
            layer.pixels().depth(),
        );
        let mut writer = crate::selection::NativeRows::new(w, h, depth, store.clone())
            .map_err(|e| e.to_string())?;
        let mut row = ScratchBytes::new(base.budget(), w as usize * 8, w as usize * 8)
            .map_err(|e| e.to_string())?;
        let mut hit_row = ScratchBytes::new(base.budget(), w as usize * 4, w as usize * 4)
            .map_err(|e| e.to_string())?;
        let appending = parameters.mode != "replace" && layer.mask().is_some();
        let [hit_x, hit_y] = sampled_origin;
        let band = (MAX_REGION_PIXELS / w as usize) as i32;
        for top in (0..h as i32).step_by(band as usize) {
            let end = (top + band).min(h as i32);
            let hit_region = Region::new([-hit_x, top - hit_y, w as i32 - hit_x, end - hit_y])
                .map_err(|e| e.to_string())?;
            let hits = sampled.surface().prepare_region(hit_region, &mut || guard())
                .map_err(|e| e.to_string())?;
            for y in top..end {
                guard().map_err(|e| e.to_string())?;
                let old = if parameters.mode == "replace" {
                    None
                } else {
                    let region = Region::new([0, y, w as i32, y + 1]).map_err(|e| e.to_string())?;
                    Some(source.prepare_region(region, guard)?)
                };
                hits.copy_row(y - hit_y, &mut hit_row).map_err(|e| e.to_string())?;
                for (x, (pixel, coverage)) in row.chunks_exact_mut(8).zip(hit_row.chunks_exact(4)).enumerate() {
                    let hit = coverage[0] > 0;
                    let previous = if let Some(old) = &old {
                        if depth == 16 {
                            (old.value16(x as i32, y)? * 65535.).round() as u16
                        } else {
                            (old.value8(x as i32, y)? * 255.).round() as u16 * 257
                        }
                    } else {
                        0
                    };
                    let value = match (parameters.mode, hit) {
                        ("subtract", true) => 0,
                        (_, true) => 65535,
                        _ => previous,
                    };
                    let alpha = if appending || value > 0 { 65535 } else { 0 };
                    for (pair, value) in pixel.chunks_exact_mut(2).zip([value, value, value, alpha]) {
                        pair.copy_from_slice(&value.to_be_bytes());
                    }
                }
                writer
                    .row(y as u32, &row, &mut || guard())
                    .map_err(|e| e.to_string())?;
            }
        }
        writer.finish(&mut || guard()).map_err(|e| e.to_string())
    });
    let output = super::mask::finish(result, Some(sampled), session)?;
    match edit.append_mask(token, output, &checked) {
        Ok(delta) => Ok(delta),
        Err(refused) => {
            let reason = refused.reason().to_owned();
            Err(cleanup(
                reason,
                refused.abort(session, &|| session.cleanup_check()),
            ))
        }
    }
}

fn sample(
    guide: &Surface,
    parameters: &Parameters<'_>,
    store: crate::large_document::Store,
    check: &dyn Fn() -> crate::large_document::Result<()>,
) -> Result<CoverageSource> {
    let (w, h) = (guide.width(), guide.height());
    let [x, y] = parameters.point.map(|v| v as i32);
    let seed = guide
        .prepare_region(
            Region::new([x, y, x + 1, y + 1]).map_err(|e| e.to_string())?,
            &mut || check(),
        )
        .and_then(|view| view.pixel(x, y))
        .map_err(|e| e.to_string())?;
    let band_height = (MAX_REGION_PIXELS / w as usize) as u32;
    let pin_band = |y: u32| {
        let top = y / band_height * band_height;
        let bottom = (top + band_height).min(h);
        guide.prepare_region(Region::new([0, top as i32, w as i32, bottom as i32])?, &mut || check())
    };
    if parameters.contiguous {
        let band = RefCell::new(None::<SurfaceRegion>);
        return worker::prepare(
            &worker::Request {
                width: w,
                height: h,
                seeds: &[parameters.point],
                quick_radius: None,
                feather: 0,
                mode: Combine::Replace,
                maximum_work: 64_000_000.max(u64::from(w) * u64::from(h) * 4),
            },
            store,
            None,
            check,
            |y| {
                let mut band = band.borrow_mut();
                if band.as_ref().is_none_or(|view| {
                    let [_, top, _, bottom] = view.region().bounds();
                    !(top..bottom).contains(&(y as i32))
                }) {
                    drop(band.take());
                    *band = Some(pin_band(y)?);
                }
                Ok(())
            },
            |_, x, y| {
                let band = band.borrow();
                Ok(parameters.matches(
                    band.as_ref()
                        .ok_or(Error::Invalid("Color mask guide band missing"))?
                        .pixel(x as i32, y as i32)?,
                    seed,
                ))
            },
        );
    }
    let pool = store.resident_budget();
    let mut band = None;
    let mut pixels = ScratchBytes::new(&pool, w as usize * 4, w as usize * 4)
        .map_err(|e| e.to_string())?;
    crate::selection::stage_rows(
        w,
        h,
        store,
        &pool,
        check,
        |y, row| {
            if y % band_height == 0 {
                drop(band.take());
                band = Some(pin_band(y).map_err(|e| e.to_string())?);
            }
            band.as_ref().ok_or("Color mask guide band missing")?
                .copy_row(y as i32, &mut pixels)
                .map_err(|e| e.to_string())?;
            for (value, pixel) in row.iter_mut().zip(pixels.chunks_exact(4)) {
                let color = [pixel[0], pixel[1], pixel[2], pixel[3]];
                *value = if parameters.matches(color, seed) { 255 } else { 0 };
            }
            Ok(())
        },
    )
}

fn colors(
    base: &NativeDocument,
    layer: LayerMetadata<'_>,
    session: &SourceSession,
    check: &dyn Fn() -> Result<()>,
) -> Result<StagedOutput> {
    let (w, h) = (layer.pixels().width(), layer.pixels().height());
    let origin = layer.origin();
    // Resident8 folders without effects use the color-managed isolated preview,
    // then crop it into the layer's local frame. All other cases use raw colors.
    let display = base.metadata().depth() == 8
        && layer.kind() == "group"
        && !layer
            .effects()
            .any(|effect| effect.enabled() && effect.weight() > 0.);
    if display {
        let [x, y] = origin;
        let frame = [x, y, x + w as i32, y + h as i32];
        return crate::render::native_preview::stage_layer_guide(base, session, frame, layer.id(), check);
    }
    let _header = base
        .budget()
        .reserve_bytes(base.metadata().id().len() + 1024, 4096)
        .map_err(|e| e.to_string())?;
    let scene = SceneSnapshot::from_native(
        SceneIdentity {
            project_id: String::new(),
            document_id: base.metadata().id().into(),
            revision: base.metadata().revision(),
        },
        base.clone(),
        vec![],
        None,
        None,
    )?;
    let topology = Topology::new(&scene)?;
    let index = base
        .metadata()
        .layers()
        .position(|l| l.id() == layer.id())
        .ok_or("Color mask layer missing")?;
    let stored = if Stored::needed(&topology) {
        Some(Stored::prepare(topology.clone(), session, &mut || check())?)
    } else {
        None
    };
    let result = crate::retained::transaction::stage(base, session, check, |store, checked| {
        let mut writer =
            crate::selection::NativeRows::new(w, h, 8, store.clone()).map_err(|e| e.to_string())?;
        let mut row = ScratchBytes::new(base.budget(), w as usize * 4, w as usize * 4)
            .map_err(|e| e.to_string())?;
        let stripe = (MAX_REGION_PIXELS / w as usize).min(16);
        for top in (0..h as i32).step_by(stripe) {
            let bottom = (top + stripe as i32).min(h as i32);
            let region = Region::new([
                origin[0],
                top + origin[1],
                origin[0] + w as i32,
                bottom + origin[1],
            ])
            .map_err(|e| e.to_string())?;
            let prepared =
                LayerWindow::prepare(&topology, region, stored.as_ref(), &mut || check())?;
            let p8 = if layer.pixels().depth() == 8 {
                Some(Plan::<Native8, _>::with_topology(
                    topology.clone(),
                    &prepared,
                    region,
                )?)
            } else {
                None
            };
            let p16 = if layer.pixels().depth() == 16 {
                Some(Plan::<Native16, _>::with_topology(
                    topology.clone(),
                    &prepared,
                    region,
                )?)
            } else {
                None
            };
            for y in top..bottom {
                checked().map_err(|e| e.to_string())?;
                for (x, pixel) in row.chunks_exact_mut(4).enumerate() {
                    let (x, y) = (x as i32 + origin[0], y + origin[1]);
                    let color = if let Some(plan) = &p8 {
                        plan.layer(index, x, y)?
                    } else {
                        p16.as_ref()
                            .ok_or("Color mask precision missing")?
                            .layer(index, x, y)?
                            .map(|v| ((u32::from(v) + 128) / 257) as u8)
                    };
                    pixel.copy_from_slice(&color);
                }
                writer
                    .row8(y as u32, &row, &mut || checked())
                    .map_err(|e| e.to_string())?;
            }
        }
        writer.finish(&mut || checked()).map_err(|e| e.to_string())
    });
    let retired = stored.map_or(Ok(()), |stored| {
        stored.retire(&mut || session.cleanup_check())
    });
    match (result, retired) {
        (result, Ok(())) => result,
        (Err(reason), Err(retired)) => Err(cleanup(reason, Err(retired))),
        (Ok(output), Err(reason)) => Err(cleanup(
            reason,
            output.abort(session, &|| session.cleanup_check()),
        )),
    }
}
