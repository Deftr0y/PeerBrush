//! Shared command preparation for UI previews, MCP and CLI edits. No step
//! publishes sources, changes the live document, or enters history.
use super::*;
use crate::{engine::authority::NativeCapture, workspace::source_runtime::SourceSession};

pub(crate) struct PreparedNative {
    document: NativeDocument,
    history: HistorySnapshot,
    delta: PreparedDelta,
}
#[derive(Debug)]
pub(crate) struct PreparationFailure {
    pub reason: String,
    pub cleanup_error: Option<String>,
}
impl PreparationFailure {
    fn new(reason: String) -> Self {
        Self {
            reason,
            cleanup_error: None,
        }
    }
    fn cleanup(&mut self, result: Result<()>) {
        if let Err(reason) = result {
            self.cleanup_error = Some(match self.cleanup_error.take() {
                None => reason,
                Some(previous) => format!("{previous}; {reason}"),
            });
        }
    }
}
impl PreparedNative {
    pub(crate) fn preview(&self) -> &NativeDocument {
        self.delta.preview()
    }
    pub(crate) fn into_parts(self) -> (NativeDocument, HistorySnapshot, PreparedDelta) {
        (self.document, self.history, self.delta)
    }
    pub(crate) fn abort(
        self,
        session: &SourceSession,
        cleanup: &dyn Fn() -> Result<()>,
    ) -> Result<()> {
        let Self {
            document,
            history,
            delta,
        } = self;
        drop((document, history));
        delta.abort(session, cleanup)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Handler {
    Metadata,
    Retained,
}
/// The central route and compatibility report share this exact implementation
/// table. A missing producer never falls back to resident materialization.
pub(crate) fn handler(operation: &str) -> Option<Handler> {
    match operation {
        "layer.update" | "layer.add" | "layer.delete" | "layer.reorder" | "layer.clip"
        | "layer.parent" | "mask.remove" | "mask.toggle" | "mask.link" => Some(Handler::Metadata),
        "move" | "transform" => Some(Handler::Retained),
        _ => None,
    }
}
pub(crate) fn prepare(
    capture: NativeCapture,
    session: &SourceSession,
    check: &dyn Fn() -> Result<()>,
) -> std::result::Result<PreparedNative, PreparationFailure> {
    check()
        .and_then(|_| session.check())
        .map_err(PreparationFailure::new)?;
    let (document, history, edit) = capture.into_parts();
    if !document.budget().same_pool(&session.resident())
        || !document.disk_budget().same_pool(&session.disk())
    {
        return Err(PreparationFailure::new(
            "Native worker uses another source admission".into(),
        ));
    }
    // Refuse unsupported operations before any earlier batch step writes files.
    // Handler coverage expands only with the actual nominal producer below.
    for command in edit
        .descriptor()
        .ok_or_else(|| PreparationFailure::new("Native worker requires normalized capture".into()))?
        .commands()
    {
        let operation = command.parameters()["op"]
            .as_str()
            .ok_or_else(|| PreparationFailure::new("Native worker operation is missing".into()))?;
        if handler(operation).is_none() {
            return Err(PreparationFailure::new(
                "Native command has no connected bounded producer".into(),
            ));
        }
    }
    let mut transaction = edit.transaction().map_err(PreparationFailure::new)?;
    let count = transaction.captured_command_count();
    for _ in 0..count {
        let step = match transaction.next(check) {
            Ok(step) => step,
            Err(reason) => {
                let mut failure = PreparationFailure::new(reason);
                failure.cleanup(transaction.abort(session, &|| session.cleanup_check()));
                return Err(failure);
            }
        };
        let delta = match prepare_step(step, session, check) {
            Ok(delta) => delta,
            Err(reason) => {
                let mut failure = PreparationFailure::new(reason);
                failure.cleanup(transaction.abort(session, &|| session.cleanup_check()));
                return Err(failure);
            }
        };
        if let Err(refused) = transaction.complete(delta, check) {
            let mut failure = PreparationFailure::new(refused.reason().to_owned());
            failure.cleanup(refused.abort(session, &|| session.cleanup_check()));
            failure.cleanup(transaction.abort(session, &|| session.cleanup_check()));
            return Err(failure);
        }
    }
    match transaction.finish(check) {
        Ok(delta) => Ok(PreparedNative {
            document,
            history,
            delta,
        }),
        Err(refused) => {
            let mut failure = PreparationFailure::new(refused.reason().to_owned());
            failure.cleanup(refused.abort(session, &|| session.cleanup_check()));
            Err(failure)
        }
    }
}
fn prepare_step(
    edit: CapturedEdit,
    session: &SourceSession,
    check: &dyn Fn() -> Result<()>,
) -> Result<PreparedDelta> {
    session.check()?;
    check()?;
    let command = edit
        .descriptor()
        .ok_or("Native step descriptor missing")?
        .commands()
        .next()
        .ok_or("Native step command missing")?;
    let token = command.token();
    let kind = handler(
        command.parameters()["op"]
            .as_str()
            .ok_or("Native operation missing")?,
    )
    .ok_or("Native producer missing")?;
    match kind {
        Handler::Metadata => edit.metadata_command(token, check),
        Handler::Retained => {
            // Preserve the existing operation work admission until the complete
            // large-document command envelope is qualified and activated.
            let mut work = crate::transform::native::Work::new(128_000_000)?;
            crate::retained::transaction::prepare_command(edit, token, session, &mut work, check)
        }
    }
}

#[cfg(test)]
#[path = "worker_tests.rs"]
mod tests;
