use super::*;
use crate::{
    compositor::native::{
        prepare_filters_with_layers, LayerWindow, Native16, Native8, Plan as Composite, Stored,
        Topology,
    },
    engine::scene::{SceneIdentity, SceneSnapshot},
    large_document::ScratchBytes,
    raster::{backed::Region, TILE},
    workspace::source_runtime::ReadChecks,
};
use ring::digest::{Context, SHA256};

fn scene(base: &NativeDocument) -> Result<SceneSnapshot> {
    SceneSnapshot::from_native(
        SceneIdentity {
            project_id: String::new(),
            document_id: base.metadata().id().into(),
            revision: base.metadata().revision(),
        },
        base.clone(),
        vec![],
        None,
        None,
    )
}
fn put(out: &mut [u8], p: [u16; 4]) {
    for (pair, v) in out.chunks_exact_mut(2).zip(p) {
        pair.copy_from_slice(&v.to_be_bytes());
    }
}

pub(super) fn mask(
    base: &NativeDocument,
    plan: &Plan,
    session: &SourceSession,
    check: &dyn Fn() -> Result<()>,
) -> Result<StagedOutput> {
    crate::retained::transaction::stage(base, session, check, |store, guard| {
        let [w, h] = plan.size;
        let layer = base
            .metadata()
            .layer(plan.index)
            .ok_or("Bake layer missing")?;
        let mut writer =
            crate::selection::NativeRows::new(w, h, base.metadata().depth(), store.clone())
                .map_err(|e| e.to_string())?;
        let mut row = ScratchBytes::new(base.budget(), w as usize * 8, w as usize * 8)
            .map_err(|e| e.to_string())?;
        let stripe = (crate::raster::backed::MAX_REGION_PIXELS / w as usize).min(TILE as usize);
        for top in (0..h as i32).step_by(stripe) {
            check()?;
            let bottom = (top + stripe as i32).min(h as i32);
            let [ox, oy] = layer.origin();
            let values = crate::mask::region::prepare_bake(
                base,
                layer,
                [ox, oy + top, ox + w as i32, oy + bottom],
                plan.at + 1,
                plan.quantized,
                &mut ReadChecks::caller_only(&mut || check()),
            )?;
            for y in top..bottom {
                check()?;
                for (x, pixel) in row.chunks_exact_mut(8).enumerate() {
                    let v = if base.metadata().depth() == 16 {
                        (values.value16(x as i32, y)? * 65535.).round() as u16
                    } else {
                        (values.value8(x as i32, y)? * 255.).round() as u16 * 257
                    };
                    put(pixel, [v, v, v, 65535]);
                }
                writer
                    .row(y as u32, &row, &mut || guard())
                    .map_err(|e| e.to_string())?;
            }
        }
        writer.finish(&mut || guard()).map_err(|e| e.to_string())
    })
}

pub(super) fn color(
    base: &NativeDocument,
    plan: &Plan,
    session: &SourceSession,
    check: &dyn Fn() -> Result<()>,
) -> Result<StagedOutput> {
    let scene = scene(base)?;
    let topology = Topology::new(&scene)?;
    let stored = if Stored::needed(&topology) {
        Some(Stored::prepare(topology.clone(), session, &mut || check())?)
    } else {
        None
    };
    let result = crate::retained::transaction::stage(base, session, check, |store, guard| {
        let [w, h] = plan.size;
        let [ox, oy] = plan.origin;
        let mut writer =
            crate::selection::NativeRows::new(w, h, base.metadata().depth(), store.clone())
                .map_err(|e| e.to_string())?;
        let mut row = ScratchBytes::new(base.budget(), w as usize * 8, w as usize * 8)
            .map_err(|e| e.to_string())?;
        let stripe = (crate::raster::backed::MAX_REGION_PIXELS / w as usize).min(TILE as usize);
        for top in (0..h as i32).step_by(stripe) {
            let bottom = (top + stripe as i32).min(h as i32);
            let region = Region::new([ox, oy + top, ox + w as i32, oy + bottom])
                .map_err(|e| e.to_string())?;
            let window = LayerWindow::prepare(&topology, region, stored.as_ref(), &mut || check())?;
            let p8 = if base.metadata().depth() == 8 {
                Some(Composite::<Native8, _>::with_topology(
                    topology.clone(),
                    &window,
                    region,
                )?)
            } else {
                None
            };
            let p16 = if base.metadata().depth() == 16 {
                Some(Composite::<Native16, _>::with_topology(
                    topology.clone(),
                    &window,
                    region,
                )?)
            } else {
                None
            };
            for y in top..bottom {
                check()?;
                for (x, pixel) in row.chunks_exact_mut(8).enumerate() {
                    let p = if let Some(p) = &p16 {
                        p.layer(plan.index, ox + x as i32, oy + y)?
                    } else {
                        p8.as_ref()
                            .ok_or("Bake precision missing")?
                            .layer(plan.index, ox + x as i32, oy + y)?
                            .map(|v| u16::from(v) * 257)
                    };
                    put(pixel, p);
                }
                writer
                    .row(y as u32, &row, &mut || guard())
                    .map_err(|e| e.to_string())?;
            }
        }
        writer.finish(&mut || guard()).map_err(|e| e.to_string())
    });
    let retired = stored.map_or(Ok(()), |stored| {
        stored.retire(&mut || session.cleanup_check())
    });
    match (result, retired) {
        (result, Ok(())) => result,
        (Err(reason), Err(cleanup)) => Err(super::super::worker::cleanup(reason, Err(cleanup))),
        (Ok(output), Err(reason)) => Err(super::super::worker::cleanup(
            reason,
            output.abort(session, &|| session.cleanup_check()),
        )),
    }
}

/// Canonical native words bind raw target color, raw mask preview and the
/// filtered composite. The isolated comparison separately removes occlusion.
pub(super) fn fingerprints(
    base: &NativeDocument,
    target: Option<&str>,
    session: &SourceSession,
    check: &dyn Fn() -> Result<()>,
) -> Result<[[u8; 32]; 3]> {
    let scene = scene(base)?;
    let topology = Topology::new(&scene)?;
    let index = target
        .map(|id| {
            base.metadata()
                .layers()
                .position(|l| l.id() == id)
                .ok_or("Bake target disappeared")
        })
        .transpose()?;
    let stored = if Stored::composite_needed(&topology, None) {
        Some(Stored::prepare_composite(topology.clone(), session, None, &mut || check())?)
    } else {
        None
    };
    let result = (|| {
        let w = base.metadata().width();
        let h = base.metadata().height();
        let mut row = ScratchBytes::new(base.budget(), w as usize * 24, w as usize * 24)
            .map_err(|e| e.to_string())?;
        let mut digests = std::array::from_fn::<_, 3, _>(|_| Context::new(&SHA256));
        let stripe = (crate::raster::backed::MAX_REGION_PIXELS / w as usize).min(TILE as usize);
        for top in (0..h as i32).step_by(stripe) {
            check()?;
            let bottom = (top + stripe as i32).min(h as i32);
            let region = Region::new([0, top, w as i32, bottom]).map_err(|e| e.to_string())?;
            let filtered =
                prepare_filters_with_layers(&topology, region, stored.as_ref(), &mut || check())?;
            let window = LayerWindow::prepare(&topology, region, stored.as_ref(), &mut || check())?;
            let p8 = if base.metadata().depth() == 8 {
                Some(Composite::<Native8, _>::with_topology(
                    topology.clone(),
                    &window,
                    region,
                )?)
            } else {
                None
            };
            let p16 = if base.metadata().depth() == 16 {
                Some(Composite::<Native16, _>::with_topology(
                    topology.clone(),
                    &window,
                    region,
                )?)
            } else {
                None
            };
            for y in top..bottom {
                check()?;
                for x in 0..w as i32 {
                    let (color, mask, composite) = if let Some(p) = &p16 {
                        let color = index.map_or(Ok([0; 4]), |i| p.layer(i, x, y))?;
                        let mask = index.map_or(Ok(65535), |i| {
                            p.mask(i, x, y, true).map(|v| (v * 65535.).round() as u16)
                        })?;
                        let composite = if let Some(f) = &filtered {
                            f.pixel16(x, y)?
                        } else {
                            p.pixel(x, y)?
                        };
                        (color, mask, composite)
                    } else {
                        let p = p8.as_ref().ok_or("Bake comparison precision missing")?;
                        let color = index
                            .map_or(Ok([0; 4]), |i| p.layer(i, x, y))?
                            .map(|v| u16::from(v) * 257);
                        let mask = index.map_or(Ok(65535), |i| {
                            p.mask(i, x, y, true)
                                .map(|v| u16::from((v * 255.) as u8) * 257)
                        })?;
                        let composite = if let Some(f) = &filtered {
                            f.pixel(x, y)?
                        } else {
                            p.pixel(x, y)?
                        };
                        (color, mask, composite.map(|v| u16::from(v) * 257))
                    };
                    let at = x as usize * 8;
                    let stride = w as usize * 8;
                    put(&mut row[at..at + 8], color);
                    put(
                        &mut row[stride + at..stride + at + 8],
                        [mask, mask, mask, 65535],
                    );
                    put(&mut row[2 * stride + at..2 * stride + at + 8], composite);
                }
                for (digest, channel) in digests.iter_mut().zip(row.chunks_exact(w as usize * 8)) {
                    digest.update(channel);
                }
            }
        }
        check()?;
        Ok(digests.map(|d| d.finish().as_ref().try_into().unwrap()))
    })();
    let retired = stored.map_or(Ok(()), |stored| {
        stored.retire(&mut || session.cleanup_check())
    });
    match (result, retired) {
        (result, Ok(())) => result,
        (Err(reason), Err(cleanup)) => Err(super::super::worker::cleanup(reason, Err(cleanup))),
        (Ok(_), Err(reason)) => Err(reason),
    }
}
