//! Shared off-UI editing. Source preparation and retirement never hold Engine.
use super::*;
use crate::clipboard::{native::PasteImage, LayerSnapshot};
use crate::engine::native::{self, HistoryRequest};
use std::sync::atomic::AtomicU8;
#[path = "native_edit/history.rs"]
mod history;
#[path = "native_edit/reply.rs"]
mod response;
pub(crate) use history::{history_shared, history_shared_controlled};
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
    pub(crate) fn start(&self) -> Result<Run<'_>, String> {
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
    pub(crate) fn check(&self) -> Result<(), String> {
        if self.0.load(Ordering::Acquire) == CANCELLED {
            Err("Edit cancelled; current work preserved".into())
        } else {
            Ok(())
        }
    }
    pub(crate) fn begin_commit(&self) -> Result<(), String> {
        self.0
            .compare_exchange(RUNNING, COMMITTING, Ordering::AcqRel, Ordering::Acquire)
            .map(|_| ())
            .map_err(|_| "Edit cancelled before commit; current work preserved".into())
    }
}
pub(crate) struct Run<'a>(&'a EditControl);
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
        None,
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
    source: Option<crate::engine::view::DocumentView<'_>>,
    control: &EditControl,
) -> Result<EditReply, String> {
    dispatch(
        shared,
        actor,
        commands,
        expected,
        task,
        label,
        gesture,
        document_id,
        control,
        None,
        source,
        None,
    )
}

pub(crate) fn edit_shared_observed(
    shared: &Shared,
    actor: &str,
    commands: &[Value],
    source: crate::engine::view::DocumentView<'_>,
    task: Option<&str>,
    label: &str,
) -> Result<EditReply, String> {
    dispatch(
        shared,
        actor,
        commands,
        Some(source.revision()),
        task,
        label,
        None,
        Some(source.id()),
        &EditControl::default(),
        None,
        Some(source),
        None,
    )
}

/// Keep source owners typed and alive through preparation and publication.
pub(crate) fn paste_layers_shared(
    shared: &Shared,
    actor: &str,
    snapshot: &LayerSnapshot,
    target: &str,
    expected: Option<u64>,
    task: Option<&str>,
    document_id: Option<&str>,
    source: Option<crate::engine::view::DocumentView<'_>>,
) -> Result<EditReply, String> {
    dispatch(
        shared,
        actor,
        &[json!({"op":"layer.paste","layer":target})],
        expected,
        task,
        "Paste layers",
        None,
        document_id,
        &EditControl::default(),
        Some(Clipboard::Layers(snapshot)),
        source,
        None,
    )
}

pub(crate) fn paste_image_shared(
    shared: &Shared,
    image: PasteImage<'_>,
    target: &str,
    revision: u64,
    document: &str,
    source: Option<crate::engine::view::DocumentView<'_>>,
) -> Result<EditReply, String> {
    dispatch(
        shared,
        "human",
        &[json!({"op":"image.paste","layer":target,"origin":image.origin()})],
        Some(revision),
        None,
        "Paste images",
        None,
        Some(document),
        &EditControl::default(),
        Some(Clipboard::Image(image)),
        source,
        None,
    )
}

pub(crate) fn paste_resident_images_shared(
    shared: &Shared,
    commands: &[Value],
    pixels: Vec<crate::raster::Raster>,
    revision: u64,
    document: &str,
    source: Option<crate::engine::view::DocumentView<'_>>,
) -> Result<EditReply, String> {
    dispatch(
        shared, "human", commands, Some(revision), None, "Paste images", None, Some(document),
        &EditControl::default(), Some(Clipboard::ResidentImages(pixels)), source, None,
    )
}

/// Code keeps its exact source, task and narrower declared scopes through the
/// same native transaction and commit fence as ordinary UI/protocol editing.
pub(crate) fn edit_code_shared(
    shared: &Shared,
    job: &crate::code::Job,
    source: &native::NativeDocument,
    commands: &[Value],
    started: std::time::Instant,
) -> Result<EditReply, String> {
    let (actor, task, label) = job.attribution();
    dispatch(
        shared,
        actor,
        commands,
        Some(source.metadata().revision()),
        Some(task),
        label,
        None,
        Some(source.metadata().id()),
        &EditControl::default(),
        None,
        Some(source.into()),
        Some(CodePolicy { job, started }),
    )
}
struct CodePolicy<'a> {
    job: &'a crate::code::Job,
    started: std::time::Instant,
}
impl CodePolicy<'_> {
    fn check(
        &self,
        engine: &mut Engine,
        source: Option<crate::engine::view::DocumentView<'_>>,
    ) -> Result<(), String> {
        self.job.check_edit(
            engine,
            source.ok_or("Code publication requires its frozen source")?,
            self.started,
        )
    }
}

enum Clipboard<'a> {
    Layers(&'a LayerSnapshot),
    Image(PasteImage<'a>),
    ResidentImages(Vec<crate::raster::Raster>),
}

fn command_source_session(
    shared: &Shared,
    scene: &crate::engine::scene::SceneSnapshot,
    check: impl Fn() -> Result<(), String>,
) -> Result<crate::workspace::source_runtime::SourceSession, String> {
    use crate::workspace::native_dispatch::{begin_edit_scene, retryable_source_read};
    use std::time::{Duration, Instant};

    let deadline = Instant::now() + Duration::from_secs(1);
    loop {
        check()?;
        match begin_edit_scene(shared, scene) {
            // Wait before preparing a candidate. Each attempt checks the same source.
            Err(error) if retryable_source_read(&error) && Instant::now() < deadline => {
                thread::sleep(Duration::from_millis(10));
            }
            result => return result,
        }
    }
}

#[cfg(test)]
#[test]
fn command_source_admission_retries_the_same_project_after_contention() {
    use crate::engine::{authority::DocumentAuthority, native::worker::tests::Fixture};
    use std::{cell::Cell, sync::mpsc, time::Duration};

    let fixture = Fixture::new();
    let (runtime, _connection) = fixture.runtime();
    let shared = Arc::new(Mutex::new(Engine::new()));
    let registry = crate::workspace::attach(&shared);
    let document = native::NativeDocument::new_blank(
        8, 8, 16, &runtime.resident(), &runtime.disk(), &|| Ok(()),
    ).unwrap();
    registry.lock().unwrap().source_runtime = Some(runtime);
    let mut engine = shared.lock().unwrap();
    engine.authority = DocumentAuthority::from_native(document, 4).unwrap();
    let scene = engine.capture_scene(None).unwrap().into_native().unwrap();
    let (attempted, retry) = mpsc::channel();
    let result = thread::scope(|scope| {
        let worker = scope.spawn(|| {
            let calls = Cell::new(0);
            command_source_session(&shared, &scene, || {
                if calls.replace(calls.get() + 1) == 1 {
                    attempted.send(()).unwrap();
                }
                Ok(())
            }).map(|_| ())
        });
        let observed = retry.recv_timeout(Duration::from_secs(5));
        drop(engine);
        observed.unwrap();
        worker.join().unwrap()
    });
    result.unwrap();
    assert_eq!(shared.lock().unwrap().history_lengths(), [0, 0]);
}

fn dispatch(
    shared: &Shared,
    actor: &str,
    commands: &[Value],
    expected: Option<u64>,
    task: Option<&str>,
    label: &str,
    gesture: Option<&str>,
    document_id: Option<&str>,
    control: &EditControl,
    clipboard: Option<Clipboard<'_>>,
    source: Option<crate::engine::view::DocumentView<'_>>,
    code: Option<CodePolicy<'_>>,
) -> Result<EditReply, String> {
    let _run = control.start()?;
    let _operation = FileOperation::begin(shared)?;
    let (seed, captured_scene) = {
        let mut engine = shared.lock().unwrap();
        if let Some(code) = &code {
            code.check(&mut engine, source)?;
        }
        let view = crate::engine::view::current(&engine);
        if document_id.is_some_and(|id| id != view.id())
            || expected.is_some_and(|r| r != view.revision())
            || source.is_some_and(|source| !source.same_observation(view))
        {
            return Err("Observed edit source changed; current work preserved".into());
        }
        let Some(seed) = engine.native_seed()? else {
            let target = commands
                .first()
                .and_then(|command| command["layer"].as_str())
                .unwrap_or("");
            if let Some(Clipboard::Layers(LayerSnapshot::Native(snapshot))) = clipboard {
                let destination = view.resident().ok_or("Resident destination unavailable")?;
                crate::layer_clipboard::plan_paste(destination, snapshot.members().len(), target)?;
                let id = destination.id.clone();
                let revision = destination.revision;
                let size = [destination.width, destination.height];
                drop(engine);
                let (prepared, _charge) =
                    native::materialize_layer_clipboard(snapshot, size, &|| control.check())?;
                let mut engine = shared.lock().unwrap();
                let view = crate::engine::view::current(&engine);
                if view.native().is_some() || view.id() != id || view.revision() != revision {
                    return Err("Observed edit source changed; current work preserved".into());
                }
                control.begin_commit()?;
                return engine
                    .paste_layers(actor, &prepared, target, Some(revision), task)
                    .map(EditReply::resident);
            }
            if let Some(Clipboard::Image(image)) = clipboard {
                let PasteImage::Bitmap(bitmap) = image else {
                    return Err("Observed paste destination changed; current work preserved".into());
                };
                let id = view.id().to_owned();
                let revision = view.revision();
                drop(engine);
                let (pixels, _admission) = bitmap.prepare_resident(&|| control.check())?;
                let mut engine = shared.lock().unwrap();
                let current = crate::engine::view::current(&engine);
                if current.native().is_some()
                    || current.id() != id
                    || current.revision() != revision
                    || source.is_some_and(|source| !source.same_observation(current))
                {
                    return Err("Observed paste destination changed; current work preserved".into());
                }
                control.begin_commit()?;
                return engine
                    .paste_resident_images(actor, commands, revision, vec![pixels])
                    .map(EditReply::resident);
            }
            control.begin_commit()?;
            return match clipboard {
                Some(Clipboard::Layers(LayerSnapshot::Resident(snapshot))) => engine.paste_layers(
                    actor,
                    snapshot,
                    target,
                    expected,
                    task,
                ),
                Some(Clipboard::ResidentImages(pixels)) => engine.paste_resident_images(
                    actor, commands, expected.ok_or("Prepared clipboard images need a revision")?, pixels,
                ),
                _ => engine.edit_with_gesture(actor, commands, expected, task, label, gesture),
            }
            .map(EditReply::resident);
        };
        if matches!(clipboard, Some(Clipboard::ResidentImages(_))) {
            return Err("Observed paste destination changed; current work preserved".into());
        }
        (seed, engine.capture_scene(None)?)
    };
    let scene = captured_scene.into_native()?;
    let checked = || {
        control.check()?;
        if let Some(code) = &code {
            code.job.check_worker(
                shared,
                source.ok_or("Code preparation requires its frozen source")?,
                code.started,
            )?;
        }
        Ok(())
    };
    let frozen = native::presets::freeze_shared(shared, seed.document(), commands, &checked)?;
    let commands = frozen.as_deref().map_or(commands, Vec::as_slice);
    let descriptor = native::CommandDescriptor::from_normalized(
        seed.document(),
        seed.document().metadata().revision(),
        commands,
        &checked,
    )?;
    if descriptor.commands().any(|command| {
        native::worker::handler(command.parameters()["op"].as_str().unwrap_or("")).is_none()
    }) {
        return Err("This command has no connected native worker; current work preserved".into());
    }
    let captured = {
        let mut engine = shared.lock().unwrap();
        if let Some(code) = &code {
            code.check(&mut engine, source)?;
        }
        engine.capture_native(&seed, descriptor, actor, expected, document_id)?
    };
    let session = command_source_session(shared, &scene, || control.check())?;
    drop(scene);
    let active = || {
        checked()?;
        session.check()
    };
    let prepared = match clipboard {
        Some(Clipboard::Layers(LayerSnapshot::Native(snapshot))) => {
            native::worker::prepare_layer_paste(captured, snapshot, &session, &checked)
        }
        Some(Clipboard::Layers(LayerSnapshot::Resident(snapshot))) => {
            native::worker::prepare_resident_layer_paste(captured, snapshot, &session, &checked)
        }
        Some(Clipboard::Image(image)) => {
            native::worker::prepare_image_paste(captured, image, &session, &checked)
        }
        Some(Clipboard::ResidentImages(_)) => {
            return Err("Observed paste destination changed; current work preserved".into());
        }
        None => match &code {
            Some(code) => native::worker::prepare_scoped(captured, code.job.declared_scopes(), &session, &checked),
            None => native::worker::prepare(captured, &session, &checked),
        },
    }
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
    let pending = prepared.prepare_commit(
        HistoryRequest {
            actor,
            label,
            task,
            gesture,
        },
        &active,
    );
    let pending = match pending {
        Ok(pending) => pending,
        Err((prepared, reason)) => {
            return Err(combine(
                reason,
                prepared.abort(&session, &|| session.cleanup_check()).err(),
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
            if let Some(code) = &code {
                code.check(&mut engine, source)?;
            }
            control.begin_commit()?;
            let result = engine.commit_native_with_effects(
                &seed,
                actor,
                pending.take().unwrap(),
                effects.take().unwrap(),
            );
            if result.is_ok() {
                if let Some(code) = &code {
                    let document = engine.native_document().unwrap();
                    code.job.record_applied(
                        document.key(),
                        document.metadata().revision(),
                        commands.len(),
                    );
                }
            }
            Ok(result)
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
                reply.cleanup_pending();
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

    #[test]
    fn typed_paste_dispatch_preserves_native_words_promotion_and_undo() {
        for depth in [8, 16] {
            let mut source = crate::engine::Document::new_depth(8, 8, 16).unwrap();
            source.layers[0]
                .pixels
                .set16(2, 3, [12347, 33559, 51237, 1]);
            source.layers[0].pixels.set16(4, 5, [401, 517, 65321, 0]);
            let words = source.layers[0].pixels.rgba16();
            let snapshot = LayerSnapshot::Resident(
                crate::layer_clipboard::copy(&source, &[source.layers[0].id.clone()]).unwrap(),
            );
            let mut engine = Engine::new();
            engine
                .resident_state_mut()
                .expect("resident fixture")
                .document = crate::engine::Document::new_depth(8, 8, depth).unwrap();
            let original = engine
                .resident_state()
                .expect("resident fixture")
                .document
                .clone();
            let shared = Arc::new(Mutex::new(engine));
            let reply = paste_layers_shared(
                &shared,
                "human",
                &snapshot,
                &original.layers[0].id,
                Some(original.revision),
                None,
                Some(&original.id),
                Some((&original).into()),
            )
            .unwrap();
            assert_eq!(reply.data()["applied"], 1);
            assert_eq!(reply.data()["created_roots"].as_array().unwrap().len(), 1);
            let created = reply.data()["created_roots"][0].as_str().unwrap();
            let mut engine = shared.lock().unwrap();
            let pasted = engine
                .resident_state()
                .expect("resident fixture")
                .document
                .layers
                .iter()
                .find(|l| l.id == created)
                .unwrap();
            assert_eq!(pasted.pixels.rgba16(), words);
            assert_eq!(pasted.pixels.depth, 16);
            assert_eq!(
                engine
                    .resident_state()
                    .expect("resident fixture")
                    .document
                    .bit_depth,
                16
            );
            assert_eq!(engine.history_lengths(), [1, 0]);
            assert_eq!(engine.file_operations.load(Ordering::Acquire), 0);
            engine.undo("human").unwrap();
            assert_eq!(
                engine
                    .resident_state()
                    .expect("resident fixture")
                    .document
                    .bit_depth,
                depth
            );
            assert_eq!(
                engine
                    .resident_state()
                    .expect("resident fixture")
                    .document
                    .layers
                    .len(),
                original.layers.len()
            );
            assert_eq!(
                engine
                    .resident_state()
                    .expect("resident fixture")
                    .document
                    .layers[0]
                    .id,
                original.layers[0].id
            );
            assert_eq!(
                engine
                    .resident_state()
                    .expect("resident fixture")
                    .document
                    .layers[0]
                    .pixels
                    .rgba16(),
                original.layers[0].pixels.rgba16()
            );
        }
    }

    #[test]
    fn typed_paste_dispatch_refuses_stale_sources_reservations_and_cancellation() {
        for refusal in ["document", "revision", "reservation", "cancel"] {
            let mut engine = Engine::new();
            engine
                .resident_state_mut()
                .expect("resident fixture")
                .document = crate::engine::Document::new(8, 8).unwrap();
            let original = engine
                .resident_state()
                .expect("resident fixture")
                .document
                .clone();
            let target = &original.layers[0].id;
            let snapshot = LayerSnapshot::Resident(
                crate::layer_clipboard::copy(&original, &[target.clone()]).unwrap(),
            );
            if refusal == "reservation" {
                engine
                    .reserve(
                        "agent",
                        "Keep work",
                        vec![crate::engine::Scope {
                            target: None,
                            rect: None,
                        }],
                    )
                    .unwrap();
            }
            let shared = Arc::new(Mutex::new(engine));
            let control = EditControl::default();
            if refusal == "cancel" {
                assert!(control.cancel());
            }
            assert!(dispatch(
                &shared,
                "human",
                &[json!({"op":"layer.paste","layer":target})],
                Some(if refusal == "revision" { 1 } else { 0 }),
                None,
                "Paste layers",
                None,
                Some(if refusal == "document" {
                    "replaced-document"
                } else {
                    &original.id
                }),
                &control,
                Some(Clipboard::Layers(&snapshot)),
                None,
                None,
            )
            .is_err());
            assert!(control.is_finished());
            let engine = shared.lock().unwrap();
            assert_eq!(
                serde_json::to_value(&engine.resident_state().expect("resident fixture").document)
                    .unwrap(),
                serde_json::to_value(&original).unwrap()
            );
            assert_eq!(engine.history_lengths(), [0, 0]);
            assert_eq!(engine.file_operations.load(Ordering::Acquire), 0);
        }
    }
}
