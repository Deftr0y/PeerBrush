//! Shared off-UI editing. Source preparation and retirement never hold Engine.
use super::*;
use crate::engine::native::{self, HistoryRequest};
use std::sync::atomic::AtomicU8;
#[path = "native_edit/reply.rs"]
mod response;
pub(crate) use response::EditReply;

const READY: u8 = 0;
const RUNNING: u8 = 1;
const CANCELLED: u8 = 2;
const COMMITTING: u8 = 3;
const FINISHED: u8 = 4;

/// Cancellation is a request before the commit fence, never a rollback claim.
#[derive(Clone, Default)]
pub(crate) struct EditControl(Arc<AtomicU8>);
impl EditControl {
    pub(crate) fn cancel(&self) -> bool {
        let mut state = self.0.load(Ordering::Acquire);
        loop {
            if state != READY && state != RUNNING {
                return false;
            }
            match self
                .0
                .compare_exchange(state, CANCELLED, Ordering::AcqRel, Ordering::Acquire)
            {
                Ok(_) => return true,
                Err(actual) => state = actual,
            }
        }
    }
    pub(crate) fn is_finished(&self) -> bool {
        self.0.load(Ordering::Acquire) == FINISHED
    }
    fn start(&self) -> Result<Run<'_>, String> {
        match self
            .0
            .compare_exchange(READY, RUNNING, Ordering::AcqRel, Ordering::Acquire)
        {
            Ok(_) => Ok(Run(self)),
            Err(CANCELLED) => {
                self.0.store(FINISHED, Ordering::Release);
                Err("Edit cancelled before preparation; current work preserved".into())
            }
            Err(_) => Err("Edit control has already been used".into()),
        }
    }
    fn check(&self) -> Result<(), String> {
        if self.0.load(Ordering::Acquire) == CANCELLED {
            Err("Edit cancelled; current work preserved".into())
        } else {
            Ok(())
        }
    }
    fn begin_commit(&self) -> Result<(), String> {
        self.0
            .compare_exchange(RUNNING, COMMITTING, Ordering::AcqRel, Ordering::Acquire)
            .map(|_| ())
            .map_err(|_| "Edit cancelled before commit; current work preserved".into())
    }
}
struct Run<'a>(&'a EditControl);
impl Drop for Run<'_> {
    fn drop(&mut self) {
        self.0 .0.store(FINISHED, Ordering::Release);
    }
}

pub(crate) fn edit_shared(
    shared: &Shared,
    actor: &str,
    commands: &[Value],
    expected: Option<u64>,
    task: Option<&str>,
    label: &str,
    gesture: Option<&str>,
    document_id: Option<&str>,
) -> Result<EditReply, String> {
    edit_shared_controlled(
        shared,
        actor,
        commands,
        expected,
        task,
        label,
        gesture,
        document_id,
        &EditControl::default(),
    )
}

/// Call from a worker. Resident policy stays in Engine; native policy goes
/// through the same captured descriptor/worker/history path for every caller.
pub(crate) fn edit_shared_controlled(
    shared: &Shared,
    actor: &str,
    commands: &[Value],
    expected: Option<u64>,
    task: Option<&str>,
    label: &str,
    gesture: Option<&str>,
    document_id: Option<&str>,
    control: &EditControl,
) -> Result<EditReply, String> {
    let _run = control.start()?;
    let _operation = FileOperation::begin(shared)?;
    let (seed, captured_scene) = {
        let mut engine = shared.lock().unwrap();
        let view = crate::engine::view::current(&engine);
        if document_id.is_some_and(|id| id != view.id())
            || expected.is_some_and(|r| r != view.revision())
        {
            return Err("Observed edit source changed; current work preserved".into());
        }
        let Some(seed) = engine.native_seed()? else {
            control.begin_commit()?;
            return engine
                .edit_with_gesture(actor, commands, expected, task, label, gesture)
                .map(EditReply::resident);
        };
        (seed, engine.capture_scene(None)?)
    };
    let scene = captured_scene.into_native()?;
    let descriptor = native::CommandDescriptor::from_normalized(
        seed.document(),
        seed.document().metadata().revision(),
        commands,
        &|| control.check(),
    )?;
    if descriptor.commands().any(|command| {
        native::worker::handler(command.parameters()["op"].as_str().unwrap_or("")).is_none()
    }) {
        return Err("This command has no connected native worker; current work preserved".into());
    }
    let captured = {
        let engine = shared.lock().unwrap();
        engine.capture_native(&seed, descriptor, actor, expected, document_id)?
    };
    let session = crate::workspace::native_dispatch::begin_scene(shared, &scene)?;
    drop(scene);
    let active = || {
        control.check()?;
        session.check()
    };
    let prepared = native::worker::prepare(captured, &session, &active)
        .map_err(|error| combine(error.reason, error.cleanup_error))?;
    let mut reply = match EditReply::native(&prepared, commands.len()) {
        Ok(reply) => reply,
        Err(reason) => {
            return Err(combine(
                reason,
                prepared.abort(&session, &|| session.cleanup_check()).err(),
            ))
        }
    };
    let (base, history, delta) = prepared.into_parts();
    let pending = history.prepare_commit(
        &base,
        delta,
        HistoryRequest {
            actor,
            label,
            task,
            gesture,
        },
        &active,
    );
    drop((base, history));
    let pending = match pending {
        Ok(pending) => pending,
        Err(refused) => {
            let reason = refused.reason().to_owned();
            return Err(combine(
                reason,
                refused.abort(&session, &|| session.cleanup_check()).err(),
            ));
        }
    };
    if let Err(reason) = active() {
        return Err(combine(
            reason.clone(),
            pending
                .refused(&reason)
                .abort(&session, &|| session.cleanup_check())
                .err(),
        ));
    }
    let effects = match crate::engine::runtime_value::PreparedEngineEffects::prepare(
        &seed, &pending, &active,
    ) {
        Ok(effects) => effects,
        Err(reason) => {
            return Err(combine(
                reason.clone(),
                pending
                    .refused(&reason)
                    .abort(&session, &|| session.cleanup_check())
                    .err(),
            ));
        }
    };
    let mut pending = Some(pending);
    let mut effects = Some(effects);
    let result = {
        let mut engine = shared.lock().unwrap();
        // Only pure atomic/memory operations may run under both fences. In
        // particular no request/session owner is dropped until after unlocking.
        session.with_live_fence(|| {
            control.begin_commit()?;
            Ok(engine.commit_native_with_effects(
                &seed,
                actor,
                pending.take().unwrap(),
                effects.take().unwrap(),
            ))
        })
    };
    let result = match result {
        Ok(Ok(result)) => result,
        Ok(Err(reason)) | Err(reason) => {
            // Runtime owners and refused source cleanup retire outside both locks.
            drop(effects.take());
            let refused = pending.take().unwrap().refused(&reason);
            return Err(combine(
                reason,
                refused.abort(&session, &|| session.cleanup_check()).err(),
            ));
        }
    };
    match result {
        Ok(retired) => {
            let cleanup = retired.retire(&session, &|| session.cleanup_check()).err();
            if cleanup.is_some() {
                reply.value["cleanup_warning"] =
                    Value::String("Edit applied; provisional source cleanup is pending.".into());
            }
            Ok(reply)
        }
        Err(refused) => {
            let reason = refused.reason().to_owned();
            Err(combine(
                reason,
                refused.abort(&session, &|| session.cleanup_check()).err(),
            ))
        }
    }
}
fn combine(reason: String, cleanup: Option<String>) -> String {
    match cleanup {
        Some(cleanup) => format!("{reason}; source cleanup retained debt: {cleanup}"),
        None => reason,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cancellation_has_one_commit_boundary_and_control_is_one_shot() {
        let control = EditControl::default();
        let run = control.start().unwrap();
        assert!(control.cancel());
        assert!(control.begin_commit().is_err());
        drop(run);
        assert!(control.is_finished());
        assert!(!control.cancel());
        assert!(control.start().is_err());
        let control = EditControl::default();
        let run = control.start().unwrap();
        control.begin_commit().unwrap();
        assert!(!control.cancel());
        drop(run);
        assert!(control.is_finished());
    }
    #[test]
    fn cancellation_before_worker_start_is_terminal_and_never_enters_commit() {
        let control = EditControl::default();
        assert!(control.cancel());
        assert!(control.start().is_err());
        assert!(control.is_finished());
    }
    #[test]
    fn concurrent_cancel_and_commit_have_exactly_one_winner() {
        for _ in 0..64 {
            let control = EditControl::default();
            let run = control.start().unwrap();
            let barrier = Arc::new(std::sync::Barrier::new(2));
            let other = control.clone();
            let start = barrier.clone();
            let cancellation = std::thread::spawn(move || {
                start.wait();
                other.cancel()
            });
            barrier.wait();
            let commit = control.begin_commit().is_ok();
            assert_ne!(commit, cancellation.join().unwrap());
            drop(run);
            assert!(control.is_finished());
        }
    }
    #[test]
    fn shared_resident_edit_preserves_wire_history_and_observed_source_refusal() {
        let shared = Arc::new(Mutex::new(Engine::new()));
        let (document, layer) = {
            let engine = shared.lock().unwrap();
            let view = crate::engine::view::current(&engine);
            (view.id().to_owned(), view.layer(0).unwrap().id().to_owned())
        };
        let command = json!({"op":"layer.update","layer":layer,"visible":false});
        let reply = edit_shared(
            &shared,
            "human",
            &[command.clone()],
            Some(0),
            None,
            "Hide layer",
            None,
            Some(&document),
        )
        .unwrap();
        assert_eq!(reply.data()["revision"], 1);
        assert_eq!(reply.data()["applied"], 1);
        assert!(reply.data()["created"].as_array().unwrap().is_empty());
        assert!(edit_shared(
            &shared,
            "human",
            &[command],
            Some(0),
            None,
            "Stale edit",
            None,
            Some(&document)
        )
        .is_err());
        let engine = shared.lock().unwrap();
        assert_eq!(engine.history_lengths(), [1, 0]);
        assert!(!crate::engine::view::current(&engine)
            .layer(0)
            .unwrap()
            .visible());
        assert_eq!(engine.file_operations.load(Ordering::Acquire), 0);
    }
}
