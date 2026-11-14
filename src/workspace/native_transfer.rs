//! Transfers use the ordinary clipboard and native edit workers. Source I/O
//! and preparation stay outside both project locks.
mod destination;
use super::*;
use crate::{
    clipboard::native::layers::LayerCopyPlan,
    engine::{
        authority::NativeSeed,
        native::{self, worker::PreparedNative, HistoryRequest, PreparedCommit},
        runtime_value::{
            commit_native_pair, NativeCommitSide, PreparedEngineEffects, RefusedEngineCommit,
            RetiredEngineCommit,
        },
        scene::SceneSnapshot,
    },
    layer_clipboard::Layers,
    psd::memory36::MetadataCharge,
    transform::native::Work,
    workspace::source_runtime::SourceSession,
};
pub(super) use destination::Destination;
pub(crate) use destination::NativeTransferPreview;

pub(super) struct Capture {
    seed: NativeSeed,
    cut_scene: Option<SceneSnapshot>,
    size: [u32; 2],
}

pub(super) struct Prepared {
    seed: NativeSeed,
    layers: Layers,
    cut: Option<(PreparedNative, Arc<SourceSession>)>,
    _charge: Arc<MetadataCharge>,
}

impl Capture {
    pub(super) fn from_pair(
        from: &Engine,
        to: &Engine,
        r: &Transfer<'_>,
    ) -> Result<Option<Self>, String> {
        if from.native_document().is_none() || to.native_document().is_some() {
            return Ok(None);
        }
        guard(from, r.source_document, r.source_revision)?;
        guard(to, r.destination_document, r.destination_revision)?;
        to.ensure_write_available()?;
        crate::layer_clipboard::check_copy_source(engine::view::current(from))?;
        let seed = from
            .native_seed()?
            .ok_or("Native transfer source changed")?;
        let cut_scene = if r.move_layers {
            Some(from.capture_scene(None)?.into_native()?)
        } else {
            None
        };
        let destination = to.resident()?;
        if destination.read_only {
            return Err("This PSD is read-only".into());
        }
        Ok(Some(Self {
            seed,
            cut_scene,
            size: [destination.width, destination.height],
        }))
    }

    pub(super) fn prepare(
        self,
        pair: &TransferPair,
        r: &Transfer<'_>,
        check: &dyn Fn() -> Result<(), String>,
    ) -> Result<Prepared, String> {
        let mut work = Work::new(4_000_000)?;
        let plan = LayerCopyPlan::capture(self.seed.document(), r.layers, &mut work, check)?;
        pair.with(|from, to| {
            check()?;
            validate(&self.seed, from, to, r)?;
            crate::layer_clipboard::plan_paste(to.resident()?, plan.members().len(), r.target)?;
            Ok::<_, String>(())
        })?;
        let commands = self
            .cut_scene
            .as_ref()
            .map(|_| plan.cut_commands(&mut work, check))
            .transpose()?;
        let (layers, charge) = native::materialize_layer_clipboard(&plan, self.size, check)?;
        let cut = if let Some((scene, commands)) = self.cut_scene.zip(commands) {
            let session = Arc::new(native_dispatch::begin_edit_scene(&pair.source, &scene)?);
            let active = || check().and_then(|_| session.check());
            let descriptor = native::CommandDescriptor::from_normalized(
                self.seed.document(),
                r.source_revision,
                &commands,
                &active,
            )?;
            let captured = pair.source.lock().unwrap().capture_native(
                &self.seed,
                descriptor,
                r.actor,
                Some(r.source_revision),
                Some(r.source_document),
            )?;
            let prepared = native::worker::prepare(captured, &session, check)
                .map_err(|failure| with_cleanup(failure.reason, failure.cleanup_error))?;
            Some((prepared, session))
        } else {
            None
        };
        Ok(Prepared {
            seed: self.seed,
            layers,
            cut,
            _charge: charge,
        })
    }
}

fn validate(seed: &NativeSeed, from: &Engine, to: &Engine, r: &Transfer<'_>) -> Result<(), String> {
    guard(from, r.source_document, r.source_revision)?;
    guard(to, r.destination_document, r.destination_revision)?;
    seed.validate(from)?;
    if r.move_layers {
        check_task(from, r.actor, r.source_task)?;
    }
    to.ensure_write_available()?;
    to.resident().map(drop)
}

fn check_task(engine: &Engine, actor: &str, task: Option<&str>) -> Result<(), String> {
    if let Some(task) = task {
        let now = engine::now();
        if !engine
            .leases
            .iter()
            .any(|lease| lease.id == task && lease.owner == actor && lease.expires > now)
        {
            return Err("Task reservation expired or was released".into());
        }
    }
    Ok(())
}

fn paste(to: &mut Engine, snapshot: &Layers, r: &Transfer<'_>) -> Result<Value, String> {
    to.paste_layers(
        r.actor,
        snapshot,
        r.target,
        Some(r.destination_revision),
        r.destination_task,
    )
}

fn with_cleanup(reason: String, cleanup: Option<String>) -> String {
    match cleanup {
        Some(cleanup) => format!("{reason}; source cleanup retained debt: {cleanup}"),
        None => reason,
    }
}

impl Prepared {
    pub(super) fn preview(
        self,
        pair: &TransferPair,
        r: &Transfer<'_>,
        check: &dyn Fn() -> Result<(), String>,
    ) -> Result<Document, String> {
        let rendered = (|| {
            let mut draft = pair.with(|from, to| {
                check()?;
                validate(&self.seed, from, to, r)?;
                to.resident_draft()
            })?;
            check()?;
            paste(&mut draft, &self.layers, r)?;
            check()?;
            pair.with(|from, to| validate(&self.seed, from, to, r))?;
            draft.into_resident_document()
        })();
        let cleanup = self.cut.and_then(|(prepared, session)| {
            prepared.abort(&session, &|| session.cleanup_check()).err()
        });
        match (rendered, cleanup) {
            (Ok(document), None) => Ok(document),
            (Ok(_), Some(cleanup)) => {
                Err(format!("Preview source cleanup retained debt: {cleanup}"))
            }
            (Err(reason), cleanup) => Err(with_cleanup(reason, cleanup)),
        }
    }

    pub(super) fn commit(self, pair: &TransferPair, r: &Transfer<'_>) -> Result<Value, String> {
        let Self {
            seed,
            layers,
            cut,
            _charge,
        } = self;
        let Some((cut, session)) = cut else {
            return pair.with(|from, to| {
                validate(&seed, from, to, r)?;
                let pasted = paste(to, &layers, r)?;
                Ok(transfer_reply(from, to, r, &pasted))
            });
        };
        let mut publication = Publication::prepare(
            &seed,
            cut,
            session,
            HistoryRequest {
                actor: r.actor,
                label: "Move layers to another project",
                task: r.source_task,
                gesture: None,
            },
        )?;
        let result = pair.with(|from, to| {
            validate(&seed, from, to, r)?;
            let checkpoint = to.resident_checkpoint()?;
            let pasted = paste(to, &layers, r)?;
            if let Err(reason) = publication.publish(from, &seed, r.actor) {
                // Both engines are still locked. The source declined publication,
                // so restore the exact resident edit state before either can be seen.
                checkpoint.restore(to)?;
                return Err(reason);
            }
            Ok(transfer_reply(from, to, r, &pasted))
        });
        publication.finish(result)
    }
}

struct Publication {
    session: Arc<SourceSession>,
    pending: Option<(PreparedCommit, PreparedEngineEffects)>,
    outcome: Option<Result<RetiredEngineCommit, RefusedEngineCommit>>,
}

impl Publication {
    fn prepare(
        seed: &NativeSeed,
        edit: PreparedNative,
        session: Arc<SourceSession>,
        request: HistoryRequest<'_>,
    ) -> Result<Self, String> {
        let active = || session.check();
        let cleanup = || session.cleanup_check();
        let commit = edit
            .prepare_commit(request, &active)
            .map_err(|(edit, reason)| with_cleanup(reason, edit.abort(&session, &cleanup).err()))?;
        let effects = match PreparedEngineEffects::prepare(seed, &commit, &active) {
            Ok(effects) => effects,
            Err(reason) => {
                return Err(with_cleanup(
                    reason.clone(),
                    commit
                        .refused(&reason)
                        .abort(session.as_ref(), &cleanup)
                        .err(),
                ))
            }
        };
        Ok(Self {
            session,
            pending: Some((commit, effects)),
            outcome: None,
        })
    }

    fn publish(
        &mut self,
        engine: &mut Engine,
        seed: &NativeSeed,
        actor: &str,
    ) -> Result<(), String> {
        let outcome = self.session.with_live_fence(|| {
            let (commit, effects) = self.pending.take().expect("one transfer publication");
            engine.commit_native_with_effects(seed, actor, commit, effects)
        })?;
        let refusal = outcome
            .as_ref()
            .err()
            .map(|refused| refused.reason().to_owned());
        self.outcome = Some(outcome);
        refusal.map_or(Ok(()), Err)
    }

    fn publish_pair(
        &mut self,
        from: &mut Engine,
        from_seed: &NativeSeed,
        destination: &mut Self,
        to: &mut Engine,
        to_seed: &NativeSeed,
        actor: &str,
    ) -> Result<(), String> {
        if !Arc::ptr_eq(&self.session, &destination.session) {
            return Err("Paired transfer requires the same source session".into());
        }
        let (retired_from, retired_to) = self.session.with_live_fence(|| {
            commit_native_pair(
                NativeCommitSide {
                    engine: from,
                    seed: from_seed,
                    pending: &mut self.pending,
                },
                NativeCommitSide {
                    engine: to,
                    seed: to_seed,
                    pending: &mut destination.pending,
                },
                actor,
            )
        })??;
        self.outcome = Some(Ok(retired_from));
        destination.outcome = Some(Ok(retired_to));
        Ok(())
    }

    fn finish(self, result: Result<Value, String>) -> Result<Value, String> {
        let cleanup = || self.session.cleanup_check();
        let cleanup_error = match self.outcome {
            Some(Ok(retired)) => retired.retire(self.session.as_ref(), &cleanup).err(),
            Some(Err(refused)) => refused.abort(self.session.as_ref(), &cleanup).err(),
            None => self.pending.and_then(|(commit, _effects)| {
                commit
                    .refused("Native transfer did not publish")
                    .abort(self.session.as_ref(), &cleanup)
                    .err()
            }),
        };
        match result {
            Ok(mut reply) => {
                if cleanup_error.is_some() {
                    reply["cleanup_warning"] =
                        "Transfer applied; provisional source cleanup is pending.".into();
                }
                Ok(reply)
            }
            Err(reason) => Err(with_cleanup(reason, cleanup_error)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::{
        native::worker::tests::Fixture,
        scene::{CapturedScene, LegacyTileCache},
    };

    #[test]
    fn native_to_resident_hover_and_move_preserve_samples_and_roll_back_late_refusal() {
        let fixture = Fixture::new();
        let destination = Arc::new(Mutex::new(Engine::from_document(
            Document::new(8, 8).unwrap(),
        )));
        let connection =
            crate::server::start(destination.clone(), fixture.path().join("state")).unwrap();
        connection.configure_native_sources(None, true).unwrap();
        let workspace = attach(&destination);
        let runtime = workspace.lock().unwrap().source_runtime.clone().unwrap();
        let mut document = Document::new_depth(8, 8, 16).unwrap();
        let layer = document.layers[0].id.clone();
        document.layers[0]
            .pixels
            .set16(1, 1, [12347, 23459, 34571, 65535]);
        document.layers[0]
            .pixels
            .set16(6, 6, [54321, 32109, 10987, 0]);
        document.layers[0].pixels.retained = Some(Box::new(crate::retained::Original::from(
            &document.layers[0].pixels,
        )));
        let cache = LegacyTileCache::new(&runtime.resident(), 4).unwrap();
        let staged =
            CapturedScene::from_parts(document, "native-transfer".into(), vec![], None, None)
                .stage(&runtime, &cache, &|| Ok(()))
                .unwrap();
        let native =
            native::NativeDocument::from_pending(staged.scene.into_provisional_pending().unwrap())
                .unwrap();
        let control = crate::loading::Control::default();
        let mut prepared =
            native_registration::Prepared::new(&workspace, &runtime, native, &control).unwrap();
        prepared.publish(&workspace, &control, None).unwrap();
        let source = prepared.shared.clone();
        let (source_project, source_document, source_key) = {
            let source = source.lock().unwrap();
            let native = source.native_document().unwrap();
            (
                source.project_id.clone(),
                native.metadata().id().to_owned(),
                native.key(),
            )
        };
        let (destination_project, original) = {
            let destination = destination.lock().unwrap();
            (
                destination.project_id.clone(),
                destination.resident().unwrap().clone(),
            )
        };
        let layers = [layer.clone()];
        let request = Transfer {
            source: &source_project,
            destination: &destination_project,
            source_document: &source_document,
            destination_document: &original.id,
            source_revision: 0,
            destination_revision: original.revision,
            layers: &layers,
            target: &original.layers[0].id,
            move_layers: true,
            actor: "human",
            source_task: None,
            destination_task: None,
        };
        let preview = transfer_preview_in(&workspace, &request).unwrap();
        let (_, _, rgba, _) = preview.preview(None, 8, None, false).unwrap();
        assert_eq!(&rgba[36..40], &[48, 91, 135, 255]);
        assert_eq!(source.lock().unwrap().history_lengths(), [0, 0]);
        assert_eq!(destination.lock().unwrap().history_lengths(), [0, 0]);

        let pair = TransferPair::new(&workspace, &request).unwrap();
        let capture = pair
            .with(|from, to| Capture::from_pair(from, to, &request))
            .unwrap()
            .unwrap();
        let cut = capture.prepare(&pair, &request, &|| Ok(())).unwrap();
        let lease = source
            .lock()
            .unwrap()
            .reserve("other", "Review this layer", vec![Scope::layer(&layer)])
            .unwrap();
        assert!(cut
            .commit(&pair, &request)
            .unwrap_err()
            .contains("Reserved"));
        assert_eq!(
            source.lock().unwrap().native_document().unwrap().key(),
            source_key
        );
        assert_eq!(source.lock().unwrap().history_lengths(), [0, 0]);
        {
            let destination = destination.lock().unwrap();
            assert_eq!(destination.resident().unwrap().revision, original.revision);
            assert_eq!(destination.resident().unwrap().bit_depth, 8);
            assert_eq!(destination.history_lengths(), [0, 0]);
        }
        source
            .lock()
            .unwrap()
            .leases
            .retain(|entry| entry.id != lease.id);

        let reply = transfer_in(&workspace, &request).unwrap();
        assert_eq!(reply["moved"], true);
        assert_eq!(source.lock().unwrap().history_lengths(), [1, 0]);
        let source_view = source.lock().unwrap().doc_snapshot();
        assert!(source_view.view().find_layer(&layer).is_none());
        let mut destination = destination.lock().unwrap();
        let resident = destination.resident().unwrap();
        assert_eq!(resident.bit_depth, 16);
        let copied = reply["created_roots"][0].as_str().unwrap();
        let raster = &resident
            .layers
            .iter()
            .find(|entry| entry.id == copied)
            .unwrap()
            .pixels;
        assert_eq!(raster.get16(6, 6), [54321, 32109, 10987, 0]);
        assert_eq!(
            raster.retained.as_ref().unwrap().pixels.get16(6, 6),
            [54321, 32109, 10987, 0]
        );
        assert_eq!(destination.history_lengths(), [1, 0]);
        destination.undo("human").unwrap();
        assert_eq!(destination.resident().unwrap().bit_depth, 8);
    }
}
