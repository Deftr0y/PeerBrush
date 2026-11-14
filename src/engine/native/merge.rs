//! Selected source trees render at native precision before one atomic replacement.
use super::*;
use crate::{
    compositor::native::{LayerWindow, Native16, Native8, Plan, Stored, Topology},
    engine::scene::{SceneIdentity, SceneSnapshot},
    large_document::ScratchBytes,
    merge::input,
    raster::{backed::Region, TILE},
    workspace::source_runtime::SourceSession,
};

pub(super) fn prepare(
    edit: CapturedEdit,
    session: &SourceSession,
    check: &dyn Fn() -> Result<()>,
) -> Result<PreparedDelta> {
    let base = edit.document().clone();
    let checked = || {
        edit.cancellation.check()?;
        session.check()?;
        check()
    };
    checked()?;
    let descriptor = edit.descriptor().ok_or("Merge capture missing")?;
    let command = descriptor
        .commands()
        .next()
        .ok_or("Merge command missing")?;
    if descriptor.commands().len() != 1
        || command.method() != Method::Bake
        || command.parameters()["op"] != "layer.merge"
    {
        return Err("Merge differs from its captured command".into());
    }
    if base.metadata().read_only() {
        return Err("This PSD is read-only".into());
    }
    let c = command.parameters();
    let mut bytes = base.core.metadata.layers.len() * 128
        + base.metadata().id().len()
        + c["name"].as_str().map_or(0, str::len)
        + 8192;
    crate::effects::regional::metadata_bytes(c, 0, &mut bytes)?;
    for layer in &base.core.metadata.layers {
        bytes += layer.id.len()
            + layer.name.len()
            + layer.blend.len()
            + layer.parent.as_ref().map_or(0, String::len);
    }
    let growth = MetadataCharge::reserve(base.budget(), bytes, &checked)?;
    let ids = crate::merge::requested(c)?;
    let plan = input::Plan::new(
        base.core.metadata.layers.len(),
        |i| (&base.core.metadata.layers[i]).into(),
        &ids,
    )?;
    let top = &base.core.metadata.layers[plan.roots[0]];
    let charge = Arc::new(
        base.budget()
            .reserve_bytes(
                std::mem::size_of::<StagedOutput>() + 4096,
                std::mem::size_of::<StagedOutput>() + 4096,
            )
            .map_err(|e| e.to_string())?,
    );
    let admission = edit
        .admission
        .as_ref()
        .ok_or("Merge admission missing")?
        .with_scopes(
            &base,
            descriptor.shared_scopes(),
            charge,
            edit.admission
                .as_ref()
                .is_some_and(|a| a.is_transaction_step()),
        )?;
    let intent = edit.planned_intent(Intent::from_scopes(
        IntentKind::Capture,
        command.write_scopes(),
        base.budget(),
    )?)?;
    let mut bounds = None;
    for &i in &plan.removed {
        checked()?;
        let l = &base.core.metadata.layers[i];
        if let Some(area) = input::bounds(
            [base.metadata().width(), base.metadata().height()],
            [l.x, l.y],
            [l.pixels.width, l.pixels.height],
            &l.kind,
            l.effects
                .iter()
                .map(|e| (e.kind.as_str(), e.enabled, e.weight)),
            || content_bounds(base.roster().get(&l.pixels)?, base.budget(), &checked),
        )? {
            bounds = input::union(bounds, area);
        }
    }
    let (bounds, size) = input::frame(bounds, crate::raster::CanvasLimits::Native)?;
    let isolated = isolate(&base, &plan, &checked)?;
    super::metadata::validate_masks(&isolated.core.metadata, |r| isolated.roster().get(r))?;
    let output = render(&isolated, bounds, session, &checked)?;
    drop(isolated);
    let result = (|| {
        let ordinal = u32::try_from(base.roster().len())
            .map_err(|_| "Merge source ordinal overflow")?
            .checked_add(1)
            .ok_or("Merge source ordinal overflow")?;
        let default_name = if plan.folder_only {
            top.name.clone()
        } else {
            format!("{} merged", top.name)
        };
        let mut layer = super::import::image_layer(
            c["name"].as_str().unwrap_or(&default_name),
            top.parent.clone(),
            [bounds[0], bounds[1]],
            RasterRef36 {
                id: ordinal,
                width: size[0],
                height: size[1],
                depth: base.metadata().depth(),
                retained: None,
            },
        );
        if plan.folder_only {
            layer.blend = top.blend.clone();
        }
        let copied = copy_metadata(&base.core.metadata, base.budget(), &checked)?
            .retain_owner(&growth, &checked)?;
        let core = copied.rehome_unique(|mut metadata, _| {
            let mut i = 0;
            metadata.layers.retain(|_| {
                let keep = !plan.removed.contains(&i);
                i += 1;
                keep
            });
            metadata.layers.insert(plan.insertion, layer);
            super::metadata::validate_clipping(&metadata)?;
            let roster = rebuild_from(
                &mut metadata,
                base.roster().len() + 1,
                base.budget(),
                base.disk_budget(),
                &checked,
                |r| {
                    if r.id == ordinal {
                        Ok(output.surface().clone())
                    } else {
                        base.roster().get(r).cloned()
                    }
                },
            )?;
            metadata.revision = metadata
                .revision
                .checked_add(1)
                .ok_or("Native revision overflow")?;
            Core::prepare(
                metadata,
                roster,
                base.budget().clone(),
                base.disk_budget().clone(),
                &checked,
            )
        })?;
        checked()?;
        Ok(NativeDocument { core })
    })();
    match result {
        Ok(candidate) => Ok(PreparedDelta {
            base: base.key(),
            candidate,
            cancellation: edit.cancellation,
            intent,
            outputs: vec![output],
            admission: Some(admission),
        }),
        Err(reason) => Err(super::worker::cleanup(
            reason,
            output.abort(session, &|| session.cleanup_check()),
        )),
    }
}

fn isolate(
    base: &NativeDocument,
    plan: &input::Plan,
    check: &dyn Fn() -> Result<()>,
) -> Result<NativeDocument> {
    let core = copy_metadata(&base.core.metadata, base.budget(), check)?.rehome_unique(
        |mut metadata, _| {
            metadata.filters.clear();
            for &i in &plan.roots {
                metadata.layers[i].parent = None;
            }
            let mut i = 0;
            metadata.layers.retain(|_| {
                let keep = plan.removed.contains(&i);
                i += 1;
                keep
            });
            let roster = rebuild_from(
                &mut metadata,
                base.roster().len(),
                base.budget(),
                base.disk_budget(),
                check,
                |r| base.roster().get(r).cloned(),
            )?;
            Core::prepare(
                metadata,
                roster,
                base.budget().clone(),
                base.disk_budget().clone(),
                check,
            )
        },
    )?;
    Ok(NativeDocument { core })
}

fn content_bounds(
    source: &Surface,
    pool: &crate::large_document::ResidentBudget,
    check: &dyn Fn() -> Result<()>,
) -> Result<Option<[i32; 4]>> {
    let mut bounds = [i32::MAX, i32::MAX, i32::MIN, i32::MIN];
    for tile in source.physical_tiles() {
        check()?;
        let (tx, ty) = tile.coordinate();
        let mut interrupted = None;
        let bytes = source
            .prepare_tile((tx, ty), &mut || {
                check().map_err(|reason| {
                    interrupted = Some(reason);
                    crate::large_document::Error::Cancelled
                })
            })
            .map_err(|e| interrupted.unwrap_or_else(|| e.to_string()))?
            .copy_to_scratch(pool)
            .map_err(|e| e.to_string())?;
        for (i, p) in bytes
            .chunks_exact(usize::from(source.depth() / 2))
            .enumerate()
        {
            let visible = if source.depth() == 16 {
                p[6] != 0 || p[7] != 0
            } else {
                p[3] != 0
            };
            let x = tx * TILE + i as u32 % TILE;
            let y = ty * TILE + i as u32 / TILE;
            if !visible || x >= source.width() || y >= source.height() {
                continue;
            }
            bounds = [
                bounds[0].min(x as i32),
                bounds[1].min(y as i32),
                bounds[2].max(x as i32 + 1),
                bounds[3].max(y as i32 + 1),
            ];
        }
    }
    Ok((bounds[0] != i32::MAX).then_some(bounds))
}

fn render(
    base: &NativeDocument,
    bounds: [i32; 4],
    session: &SourceSession,
    check: &dyn Fn() -> Result<()>,
) -> Result<StagedOutput> {
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
    let stored = if Stored::needed(&topology) {
        Some(Stored::prepare(topology.clone(), session, &mut || check())?)
    } else {
        None
    };
    let result = crate::retained::transaction::stage(base, session, check, |store, guard| {
        let w = (bounds[2] - bounds[0]) as u32;
        let h = (bounds[3] - bounds[1]) as u32;
        let mut writer =
            crate::selection::NativeRows::new(w, h, base.metadata().depth(), store.clone())
                .map_err(|e| e.to_string())?;
        let mut row = ScratchBytes::new(base.budget(), w as usize * 8, w as usize * 8)
            .map_err(|e| e.to_string())?;
        let stripe = (crate::raster::backed::MAX_REGION_PIXELS / w as usize).min(TILE as usize);
        for top in (bounds[1]..bounds[3]).step_by(stripe) {
            check()?;
            let bottom = (top + stripe as i32).min(bounds[3]);
            let region =
                Region::new([bounds[0], top, bounds[2], bottom]).map_err(|e| e.to_string())?;
            let window = LayerWindow::prepare(&topology, region, stored.as_ref(), &mut || check())?;
            let p8 = if base.metadata().depth() == 8 {
                Some(Plan::<Native8, _>::with_topology(
                    topology.clone(),
                    &window,
                    region,
                )?)
            } else {
                None
            };
            let p16 = if base.metadata().depth() == 16 {
                Some(Plan::<Native16, _>::with_topology(
                    topology.clone(),
                    &window,
                    region,
                )?)
            } else {
                None
            };
            for y in top..bottom {
                check()?;
                for (x, pixel) in (bounds[0]..bounds[2]).zip(row.chunks_exact_mut(8)) {
                    let p = if let Some(plan) = &p16 {
                        plan.pixel(x, y)?
                    } else {
                        p8.as_ref()
                            .ok_or("Merge precision missing")?
                            .pixel(x, y)?
                            .map(|v| u16::from(v) * 257)
                    };
                    for (pair, value) in pixel.chunks_exact_mut(2).zip(p) {
                        pair.copy_from_slice(&value.to_be_bytes());
                    }
                }
                writer
                    .row((y - bounds[1]) as u32, &row, &mut || guard())
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
        (Err(reason), Err(cleanup)) => Err(super::worker::cleanup(reason, Err(cleanup))),
        (Ok(output), Err(reason)) => Err(super::worker::cleanup(
            reason,
            output.abort(session, &|| session.cleanup_check()),
        )),
    }
}
