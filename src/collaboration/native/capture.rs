//! Shared proposal preparation and review. Locks observe actual authority only;
//! source admission, rendering and explicit retirement run after unlocking.
use super::*;
use crate::{
    engine::{authority::NativeSeed, native::CommandDescriptor, Engine},
    server::{FileOperation, Shared},
    workspace::native_dispatch,
};
use serde_json::Value;

#[must_use = "A failed capture may still own prepared sources; retire them explicitly off-lock"]
pub(crate) enum CaptureFailure {
    Refused(String),
    Prepared(RejectedPreparation),
}
impl From<String> for CaptureFailure {
    fn from(reason: String) -> Self {
        Self::Refused(reason)
    }
}
impl CaptureFailure {
    pub(crate) fn reason(&self) -> &str {
        match self {
            Self::Refused(reason) => reason,
            Self::Prepared(refused) => &refused.reason,
        }
    }
    pub(crate) fn prepare_retirement(
        self,
    ) -> std::result::Result<Option<Retirement>, (Self, String)> {
        match self {
            Self::Refused(_) => Ok(None),
            Self::Prepared(refused) => refused
                .prepare_retirement()
                .map(Some)
                .map_err(|(refused, reason)| (Self::Prepared(refused), reason)),
        }
    }
}
impl Draft {
    /// Commands have already passed their method normalizers, like committed
    /// native edits. Capture keeps their exact frozen input and write scopes.
    pub(crate) fn prepare_shared(
        shared: &Shared,
        actor: &str,
        commands: &[Value],
        document: &str,
        revision: u64,
        label: &str,
        task: Option<&str>,
        check: &dyn Fn() -> Result<()>,
    ) -> std::result::Result<Self, CaptureFailure> {
        validate_attribution(actor, label, task)?;
        check()?;
        crate::collaboration::validate_commands(commands, check)?;
        let _operation = FileOperation::begin(shared)?;
        let (seed, captured_scene) = {
            let mut engine = shared.lock().unwrap();
            engine.expire();
            let seed = engine
                .native_seed()?
                .ok_or_else(|| "Proposal preparation requires native authority".to_owned())?;
            if seed.document().metadata().id() != document
                || seed.document().metadata().revision() != revision
            {
                return Err(CaptureFailure::Refused(
                    "The proposal source changed; observe the current project again".into(),
                ));
            }
            check_task(&engine, actor, task)?;
            (seed, engine.capture_scene(None)?)
        };
        let scene = Arc::new(captured_scene.into_native()?);
        let frozen = native::presets::freeze_shared(shared, seed.document(), commands, check)?;
        let commands = frozen.as_deref().map_or(commands, Vec::as_slice);
        let descriptor =
            CommandDescriptor::from_normalized(seed.document(), revision, commands, check)?;
        if descriptor.commands().any(|command| {
            native::worker::handler(command.parameters()["op"].as_str().unwrap_or("")).is_none()
        }) {
            return Err(CaptureFailure::Refused(
                "This proposal command has no connected native worker".into(),
            ));
        }
        let captured = capture(shared, &seed, descriptor.clone(), actor, task)?;
        let runtime = native_dispatch::runtime_for(shared)?;
        let _foreground = runtime.foreground_activity()?;
        let session = Arc::new(native_dispatch::begin_scene(shared, &scene)?);
        if !session.uses_runtime(&runtime) {
            return Err(CaptureFailure::Refused(
                "Proposal source service changed before preparation".into(),
            ));
        }
        let prepared = native::worker::prepare(captured, &session, check).map_err(|failure| {
            CaptureFailure::Refused(match failure.cleanup_error {
                None => failure.reason,
                Some(cleanup) => format!(
                    "{}; proposal cleanup retained debt: {cleanup}",
                    failure.reason
                ),
            })
        })?;
        let draft = Self::new(scene, prepared, session, actor, label, task, check)
            .map_err(CaptureFailure::Prepared)?;
        // Check after attribution admission too: its cancellation callbacks can
        // observe human changes. Preserve the owner for explicit retirement.
        let verified = check()
            .and_then(|_| draft.session.check())
            .and_then(|_| draft.check_runtime(shared))
            .and_then(|_| capture(shared, &seed, descriptor, actor, task));
        if let Err(reason) = verified {
            let Self {
                prepared, session, ..
            } = draft;
            return Err(CaptureFailure::Prepared(RejectedPreparation {
                reason,
                prepared,
                session,
            }));
        }
        Ok(draft)
    }

    /// Render off-lock against the live project, checking takeover/source changes
    /// during rows and once more before returning display-only feedback.
    pub(crate) fn render_shared(
        &self,
        shared: &Shared,
        rect: Option<[i32; 4]>,
        edge: u32,
        target: Option<&str>,
        mask: bool,
        selection_view: Option<&str>,
        check: &dyn Fn() -> Result<()>,
    ) -> Result<TransientFrame> {
        self.render_shared_with(shared, rect, edge, target, mask, selection_view, check, false,
            |request, check| request.render(&mut || check()))
    }
    pub(crate) fn render_shared_with<T: DisplayOutput>(
        &self,
        shared: &Shared,
        rect: Option<[i32; 4]>,
        edge: u32,
        target: Option<&str>,
        mask: bool,
        selection_view: Option<&str>,
        check: &dyn Fn() -> Result<()>,
        canvas: bool,
        render: impl FnOnce(&Request, &dyn Fn() -> Result<()>) -> Result<T>,
    ) -> Result<TransientFrame<T>> {
        check()?;
        self.check_runtime(shared)?;
        let current = self.current(shared)?;
        let active = || {
            check()?;
            self.current(shared).map(|_| ())
        };
        let frame = self.render_with(&current, rect, edge, target, mask, selection_view, &active, canvas, render)?;
        active()?;
        self.check_runtime(shared)?;
        self.current(shared)?;
        Ok(frame)
    }
    pub(super) fn check_runtime(&self, shared: &Shared) -> Result<()> {
        let runtime = native_dispatch::runtime_for(shared)?;
        if !self.session.uses_runtime(&runtime) {
            return Err("Proposal source service changed; current work preserved".into());
        }
        Ok(())
    }
    pub(super) fn current(&self, shared: &Shared) -> Result<NativeDocument> {
        let engine = native_dispatch::try_source_engine(shared)?;
        engine.ensure_open()?;
        check_task(
            &engine,
            &self.attribution.actor,
            self.attribution.task.as_deref(),
        )?;
        let document = engine
            .native_document()
            .ok_or("Proposal source is no longer native")?;
        if engine.project_id != self.scene.identity().project_id || !self.matches(document) {
            return Err("Proposal source changed; review a fresh proposal".into());
        }
        Ok(document.clone())
    }
}

#[cfg(test)]
pub(super) mod tests;

pub(super) fn check_task(engine: &Engine, actor: &str, task: Option<&str>) -> Result<()> {
    if task.is_some_and(|task| {
        !engine.leases.iter().any(|lease| {
            lease.id == task && lease.owner == actor && lease.expires > crate::engine::now()
        })
    }) {
        return Err(
            "The proposal task ended, expired or was taken over; current work preserved".into(),
        );
    }
    Ok(())
}
fn capture(
    shared: &Shared,
    seed: &NativeSeed,
    descriptor: Arc<CommandDescriptor>,
    actor: &str,
    task: Option<&str>,
) -> Result<crate::engine::authority::NativeCapture> {
    let engine = shared.lock().unwrap();
    check_task(&engine, actor, task)?;
    engine.capture_native(
        seed,
        descriptor,
        actor,
        Some(seed.document().metadata().revision()),
        Some(seed.document().metadata().id()),
    )
}
