//! Human approval publishes the frozen candidate through the ordinary native
//! Engine fence. Refusal returns its source owner without replaying commands.
use super::*;
use crate::{
    engine::{authority::NativeSeed, runtime_value::PreparedEngineEffects, Engine},
    server::{EditControl, EditReply, FileOperation, Shared},
    workspace::native_dispatch,
};

impl Draft {
    #[cfg(test)]
    pub(crate) fn accept_shared(
        self,
        shared: &Shared,
        accepted_by: &str,
        document: &str,
        revision: u64,
        control: &EditControl,
        check: &dyn Fn() -> Result<()>,
    ) -> std::result::Result<EditReply, Refused> {
        self.accept(
            shared,
            accepted_by,
            document,
            revision,
            control,
            None,
            check,
        )
    }
    pub(super) fn accept_guarded(
        self,
        shared: &Shared,
        accepted_by: &str,
        document: &str,
        revision: u64,
        control: &EditControl,
        approval: &super::store::Approval,
        check: &dyn Fn() -> Result<()>,
    ) -> std::result::Result<EditReply, Refused> {
        self.accept(
            shared,
            accepted_by,
            document,
            revision,
            control,
            Some(approval),
            check,
        )
    }
    fn accept(
        self,
        shared: &Shared,
        accepted_by: &str,
        document: &str,
        revision: u64,
        control: &EditControl,
        approval: Option<&super::store::Approval>,
        check: &dyn Fn() -> Result<()>,
    ) -> std::result::Result<EditReply, Refused> {
        let _activity = self.session.activity();
        let _run = match control.start() {
            Ok(run) => run,
            Err(reason) => {
                return Err(Refused {
                    reason,
                    draft: self,
                })
            }
        };
        if accepted_by != "human" {
            return Err(Refused {
                reason: "Proposals need explicit human acceptance".into(),
                draft: self,
            });
        }
        let _operation = match FileOperation::begin(shared) {
            Ok(operation) => operation,
            Err(reason) => {
                return Err(Refused {
                    reason,
                    draft: self,
                })
            }
        };
        let _foreground = match self.session.foreground_activity() {
            Ok(foreground) => foreground,
            Err(reason) => {
                return Err(Refused {
                    reason,
                    draft: self,
                });
            }
        };
        let capture = || -> Result<(NativeSeed, EditReply)> {
            control.check()?;
            check()?;
            self.session.check()?;
            self.check_runtime(shared)?;
            let current = self.current(shared)?;
            if current.metadata().id() != document || current.metadata().revision() != revision {
                return Err("The project changed; review a fresh proposal before accepting".into());
            }
            let descriptor = self.prepared.shared_descriptor()?;
            let (seed, observed) = {
                let mut engine = shared.lock().unwrap();
                if let Some(approval) = approval {
                    approval.validate(&mut engine)?;
                }
                let seed = engine
                    .native_seed()?
                    .ok_or("Native proposal authority changed")?;
                validate(&engine, &seed, &self.scene, &self.attribution)?;
                let observed = engine.capture_native(
                    &seed,
                    descriptor.clone(),
                    &self.attribution.actor,
                    Some(revision),
                    Some(document),
                )?;
                (seed, observed)
            };
            drop(observed);
            let reply = EditReply::native(&self.prepared, descriptor.commands().len())?;
            Ok((seed, reply))
        };
        let (seed, mut reply) = match capture() {
            Ok(captured) => captured,
            Err(reason) => {
                return Err(Refused {
                    reason,
                    draft: self,
                })
            }
        };
        let Self {
            scene,
            prepared,
            attribution,
            session,
        } = self;
        let active = || {
            control.check()?;
            check()?;
            session.check()?;
            let runtime = native_dispatch::runtime_for(shared)?;
            if !session.uses_runtime(&runtime) {
                return Err("Proposal source service changed; current work preserved".into());
            }
            let engine = shared
                .try_lock()
                .map_err(|_| "Project is busy; acceptance deferred")?;
            validate(&engine, &seed, &scene, &attribution)
        };
        let result = prepared.publish_with(
            native::HistoryRequest {
                actor: &attribution.actor,
                label: &attribution.label,
                task: attribution.task.as_deref(),
                gesture: None,
            },
            &active,
            |pending| {
                let effects = match active()
                    .and_then(|_| PreparedEngineEffects::prepare(&seed, &pending, &active))
                {
                    Ok(effects) => effects,
                    Err(reason) => return Err(pending.refused(&reason)),
                };
                let mut pending = Some(pending);
                let mut effects = Some(effects);
                let result = {
                    let mut engine = shared.lock().unwrap();
                    session.with_live_fence(|| -> Result<_> {
                        // No supplied callbacks, source I/O or owner retirement
                        // runs while the instance and Engine fences are held.
                        validate(&engine, &seed, &scene, &attribution)?;
                        let position = approval
                            .map(|approval| approval.validate(&mut engine))
                            .transpose()?;
                        control.begin_commit()?;
                        let result = engine.commit_native_with_effects(
                            &seed,
                            &attribution.actor,
                            pending.take().unwrap(),
                            effects.take().unwrap(),
                        );
                        if result.is_ok() {
                            if let (Some(approval), Some(position)) = (approval, position) {
                                approval.accepted(&mut engine, position);
                            }
                        }
                        Ok(result)
                    })
                };
                match result {
                    Ok(Ok(Ok(retired))) => Ok(retired),
                    Ok(Ok(Err(refused))) => Err(refused.into_refused()),
                    Ok(Err(reason)) | Err(reason) => {
                        drop(effects.take());
                        Err(pending.take().unwrap().refused(&reason))
                    }
                }
            },
        );
        match result {
            Err((prepared, reason)) => Err(Refused {
                reason,
                draft: Self {
                    scene,
                    prepared,
                    attribution,
                    session,
                },
            }),
            Ok(retired) => {
                drop((scene, attribution));
                if retired
                    .retire(&*session, &|| session.cleanup_check())
                    .is_err()
                {
                    reply.cleanup_pending();
                }
                Ok(reply)
            }
        }
    }
}

fn validate(
    engine: &Engine,
    seed: &NativeSeed,
    scene: &SceneSnapshot,
    attribution: &Attribution,
) -> Result<()> {
    engine.ensure_write_available()?;
    seed.validate(engine)?;
    if seed.project_id() != scene.identity().project_id
        || seed.document().key() != scene.key()
        || seed.document().metadata().id() != scene.identity().document_id
        || seed.document().metadata().revision() != scene.identity().revision
    {
        return Err("Proposal source changed; current work preserved".into());
    }
    super::capture::check_task(engine, &attribution.actor, attribution.task.as_deref())
}

#[cfg(test)]
mod tests;
