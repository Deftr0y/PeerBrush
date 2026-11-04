//! Stage complete transform plans before one guarded Engine publication.
use super::planning::{LayerPlan, Pixels, Plane};
use crate::{
    engine::{
        native::{
            CapturedEdit, Command, CommandToken, LayerPatch, Method, NativeDocument, OutputDraft,
            PreparedDelta, RasterBatch, RasterPatch, RetainedUpdate, SourceUpdate, StagedOutput,
        },
        scene::{Grid36, StageSession, Warp36},
    },
    large_document::{DiskBudget, Error, FreshFragment, ResidentBudget, Store},
    raster::Surface,
    transform::native::{self, Mapping, Work},
};
use std::cell::RefCell;

/// Private producer access. The real implementation retains the profile/source
/// operation; this grants no document publication or history authority.
pub(crate) trait Session: StageSession {
    fn resident(&self) -> ResidentBudget;
    fn disk(&self) -> DiskBudget;
    fn fresh(&self) -> Result<FreshFragment, String>;
}
impl Session for crate::workspace::source_runtime::SourceSession {
    fn resident(&self) -> ResidentBudget {
        self.resident()
    }
    fn disk(&self) -> DiskBudget {
        self.disk()
    }
    fn fresh(&self) -> Result<FreshFragment, String> {
        self.fresh_fragment()
    }
}

/// Consume the exact immutable normalized command captured by the Engine.
/// This entry point accepts no independent parameters or caller-built plans.
pub(crate) fn prepare_command(
    captured: CapturedEdit,
    token: CommandToken,
    session: &impl Session,
    work: &mut Work,
    check: &dyn Fn() -> Result<(), String>,
) -> Result<PreparedDelta, String> {
    let base = captured.document().clone();
    let command = captured
        .descriptor()
        .ok_or("Native transform needs a captured command descriptor")?
        .commands()
        .find(|command| command.token() == token)
        .ok_or("Native transform command belongs to another descriptor")?;
    if command.method() != Method::Geometry
        || !matches!(
            command.parameters()["op"].as_str(),
            Some("move" | "transform")
        )
    {
        return Err("Native retained transform requires a move or transform command".into());
    }
    if base.metadata().selection().is_some() && command.parameters()["selection_only"] != false {
        return super::selected::prepare(captured, token, session, work, check);
    }
    let id = command.parameters()["layer"]
        .as_str()
        .ok_or("Native transform needs a layer")?;
    let layer = base
        .metadata()
        .layers()
        .find(|layer| layer.id() == id)
        .ok_or("Unknown native transform layer")?;
    if layer.kind() == "adjustment" {
        return Err("Edit the adjustment effect settings or paint on its mask".into());
    }
    let cancellation = captured.cancellation();
    let failure = RefCell::new(None);
    let checked = || match cancellation
        .check()
        .and_then(|_| session.check())
        .and_then(|_| check())
    {
        Ok(()) => Ok(()),
        Err(reason) => {
            *failure.borrow_mut() = Some(reason);
            Err(Error::Cancelled)
        }
    };
    if layer.kind() == "group" {
        let planned = super::planning::folder(&base, id, command.parameters(), work, &checked);
        let plans = match planned {
            Ok(plans) => plans,
            Err(reason) => return Err(failure.into_inner().unwrap_or(reason)),
        };
        for plan in &plans.layers {
            check_write(&base, command, plan)?;
        }
        return prepare(captured, &plans.layers, session, work, check);
    }
    let planned = super::planning::layer(&base, id, command.parameters(), work, &checked);
    let plan = match planned {
        Ok(Some(plan)) => plan,
        Ok(None) => super::planning::unchanged(&base, layer)?,
        Err(reason) => return Err(failure.into_inner().unwrap_or(reason)),
    };
    check_write(&base, command, &plan)?;
    prepare(captured, &[plan], session, work, check)
}

fn check_write(
    base: &NativeDocument,
    command: Command<'_>,
    plan: &LayerPlan<'_>,
) -> Result<(), String> {
    let [x, y] = plan.origin;
    let bounds = [
        x,
        y,
        x.checked_add(i32::try_from(plan.frame[0]).map_err(|_| "Native transform frame overflow")?)
            .ok_or("Native transform bounds overflow")?,
        y.checked_add(i32::try_from(plan.frame[1]).map_err(|_| "Native transform frame overflow")?)
            .ok_or("Native transform bounds overflow")?,
    ];
    // Whole-layer and independent-mask transforms replace this complete logical
    // frame. Input halos and retained-original support remain read footprints.
    command.check_write(base, plan.layer.pixels().slot(), bounds)?;
    for plane in plan.color.iter().chain(&plan.masks) {
        command.check_write(base, plane.target, bounds)?;
    }
    Ok(())
}

/// All linked planes and layers enter one nominal packet. A caller must keep
/// its SourceSession alive through final Engine adoption or retirement.
fn prepare(
    captured: CapturedEdit,
    plans: &[LayerPlan<'_>],
    session: &impl Session,
    work: &mut Work,
    check: &dyn Fn() -> Result<(), String>,
) -> Result<PreparedDelta, String> {
    let cancellation = captured.cancellation();
    let checked = || {
        cancellation.check()?;
        session.check()?;
        check()?;
        cancellation.check()
    };
    checked()?;
    let base = captured.document();
    if !base.budget().same_pool(&session.resident())
        || !base.disk_budget().same_pool(&session.disk())
    {
        return Err("Native transform session uses another document admission".into());
    }
    let mut count = 0usize;
    for plan in plans {
        base.source(plan.layer.pixels().slot())?;
        for plane in plan.color.iter().chain(&plan.masks) {
            base.source(plane.target)?;
            if matches!(plane.pixels, Pixels::Editable { .. }) {
                // Never publish new source placement with stale raster pixels.
                return Err(
                    "Editable-source transform requires the qualified native source renderer"
                        .into(),
                );
            }
            count = count
                .checked_add(1)
                .ok_or("Native transform plane count overflow")?;
        }
    }
    let bytes = count
        .checked_mul(32 * 1024 + std::mem::size_of::<RasterPatch>())
        .and_then(|n| {
            plans
                .len()
                .checked_mul(std::mem::size_of::<LayerPatch>())
                .and_then(|m| n.checked_add(m))
        })
        .and_then(|n| n.checked_add(4096))
        .ok_or("Native transform packet overflow")?;
    let charge = base
        .budget()
        .reserve_bytes(bytes, bytes)
        .map_err(|e| e.to_string())?;
    let mut batch = RasterBatch {
        rasters: Vec::new(),
        layers: Vec::new(),
        effects: Vec::new(),
        selection: None,
    };
    batch
        .rasters
        .try_reserve_exact(count)
        .map_err(|_| "Native transform packet allocation refused")?;
    batch
        .layers
        .try_reserve_exact(plans.len())
        .map_err(|_| "Native transform layer allocation refused")?;
    if batch
        .rasters
        .capacity()
        .checked_mul(std::mem::size_of::<RasterPatch>())
        .and_then(|n| {
            batch
                .layers
                .capacity()
                .checked_mul(std::mem::size_of::<LayerPatch>())
                .and_then(|m| n.checked_add(m))
        })
        .is_none_or(|n| n > charge.bytes())
    {
        return Err("Native transform packet exceeds metadata admission".into());
    }
    let produced = (|| {
        for plan in plans {
            checked()?;
            for plane in plan.color.iter().chain(&plan.masks) {
                batch.rasters.push(stage_plane(
                    base, plane, plan.frame, session, work, &checked,
                )?);
            }
            batch.layers.push(LayerPatch {
                target: plan.layer.pixels().slot(),
                origin: Some(plan.origin),
                source: SourceUpdate::Keep,
                materialize_fill: plan.materialize_fill,
            });
        }
        checked()
    })();
    if let Err(reason) = produced {
        return Err(retire_batch(batch, session, reason));
    }
    let result = match captured.batch(batch, &checked) {
        Ok(prepared) => Ok(prepared),
        Err(refused) => {
            let reason = refused.reason().to_owned();
            match refused.abort(session, &|| Ok(())) {
                Ok(()) => Err(reason),
                Err(cleanup) => Err(format!("{reason}; provisional output retained: {cleanup}")),
            }
        }
    };
    drop(charge);
    result
}

fn stage_plane(
    base: &NativeDocument,
    plane: &Plane<'_>,
    frame: [u32; 2],
    session: &impl Session,
    work: &mut Work,
    check: &dyn Fn() -> Result<(), String>,
) -> Result<RasterPatch, String> {
    check()?;
    let (input, pose, fill) = match &plane.pixels {
        Pixels::Native { input, pose } => (input, pose, None),
        Pixels::Fill { input, pose, color } => (input, pose, Some(*color)),
        Pixels::Zero { depth } => {
            let surface = Surface::zero(frame[0], frame[1], *depth).map_err(|e| e.to_string())?;
            let output = seal(base, session, OutputDraft::new(surface, vec![]), check)?;
            return Ok(RasterPatch {
                target: plane.target,
                output,
                retained: RetainedUpdate::Clear,
                layer_origin: None,
            });
        }
        Pixels::Editable { .. } => return Err("Native source renderer is unavailable".into()),
    };
    if input.document().key() != base.key() {
        return Err("Native transform source generation differs".into());
    }
    let mapping = Mapping::retained(
        pose.frame[0],
        pose.frame[1],
        pose.depth,
        pose.matrix,
        pose.projective,
        pose.warps(),
        base.budget(),
    )?;
    let mut warps = Vec::new();
    warps
        .try_reserve_exact(pose.warps().len())
        .map_err(|_| "Native retained placement allocation refused")?;
    if warps
        .capacity()
        .checked_mul(std::mem::size_of::<Warp36>())
        .is_none_or(|n| n > 16 * 1024)
    {
        return Err("Native retained placement exceeds admission".into());
    }
    warps.extend(pose.warps().iter().map(|s| Warp36 {
        before: s.before,
        grid: Grid36 {
            rect: s.grid.rect,
            points: s.grid.points,
        },
    }));
    if let Some(color) = fill {
        let original = stage(base, session, check, |store, checked| {
            native::write_fill(pose.frame, pose.depth, color, store, work, checked)
        })?;
        let output = stage(base, session, check, |store, checked| {
            native::write_plane(original.surface(), &mapping, frame, store, work, checked)
        });
        let output = match output {
            Ok(output) => output,
            Err(reason) => {
                return Err(match original.abort(session, &|| Ok(())) {
                    Ok(()) => reason,
                    Err(cleanup) => format!("{reason}; fill original retained: {cleanup}"),
                });
            }
        };
        Ok(RasterPatch {
            target: plane.target,
            output,
            retained: RetainedUpdate::ReplaceOutput {
                source: original,
                matrix: mapping.matrix(),
                projective: pose.projective,
                warps,
            },
            layer_origin: None,
        })
    } else {
        let output = stage(base, session, check, |store, checked| {
            native::write_plane(input.source()?, &mapping, frame, store, work, checked)
        })?;
        Ok(RasterPatch {
            target: plane.target,
            output,
            retained: RetainedUpdate::Replace {
                source: input.original_slot(),
                matrix: mapping.matrix(),
                projective: pose.projective,
                warps,
            },
            layer_origin: None,
        })
    }
}

pub(crate) fn stage(
    base: &NativeDocument,
    session: &impl Session,
    check: &dyn Fn() -> Result<(), String>,
    write: impl FnOnce(
        &Store,
        &dyn Fn() -> crate::large_document::Result<()>,
    ) -> Result<Surface, String>,
) -> Result<StagedOutput, String> {
    check()?;
    let mut tokens = Vec::new();
    tokens
        .try_reserve_exact(1)
        .map_err(|_| "Native transform source token allocation refused")?;
    if tokens
        .capacity()
        .checked_mul(std::mem::size_of::<FreshFragment>())
        .is_none_or(|n| n > 4096)
    {
        return Err("Native transform source token exceeds admission".into());
    }
    let fragment = session.fresh()?;
    let failure = RefCell::new(None);
    let checked = || match session.check().and_then(|_| check()) {
        Ok(()) => Ok(()),
        Err(reason) => {
            *failure.borrow_mut() = Some(reason);
            Err(Error::Cancelled)
        }
    };
    let surface = match write(fragment.store(), &checked) {
        Ok(surface) => surface,
        Err(reason) => {
            return Err(retire_fragment(
                session,
                fragment,
                failure.into_inner().unwrap_or(reason),
            ))
        }
    };
    if surface.is_known_zero() {
        if let Err(reason) = session.abort_fragment(fragment, &|| Ok(())) {
            return Err(format!("Empty transform fragment retained: {reason}"));
        }
    } else {
        tokens.push(fragment);
    }
    seal(base, session, OutputDraft::new(surface, tokens), check)
}
fn seal(
    base: &NativeDocument,
    session: &impl Session,
    draft: OutputDraft,
    check: &dyn Fn() -> Result<(), String>,
) -> Result<StagedOutput, String> {
    match draft.seal(base, session, check) {
        Ok(output) => Ok(output),
        Err(refused) => {
            let reason = refused.reason().to_owned();
            match refused.into_draft().abort(session, &|| Ok(())) {
                Ok(()) => Err(reason),
                Err(cleanup) => Err(format!("{reason}; provisional output retained: {cleanup}")),
            }
        }
    }
}
fn retire_fragment(session: &impl Session, fragment: FreshFragment, reason: String) -> String {
    match session.abort_fragment(fragment, &|| Ok(())) {
        Ok(()) => reason,
        Err(cleanup) => format!("{reason}; provisional output retained: {cleanup}"),
    }
}
fn retire_batch(batch: RasterBatch, session: &impl Session, reason: String) -> String {
    match batch.abort(session, &|| Ok(())) {
        Ok(()) => reason,
        Err(cleanup) => format!("{reason}; provisional output retained: {cleanup}"),
    }
}

#[cfg(test)]
#[path = "transaction/tests.rs"]
mod tests;
