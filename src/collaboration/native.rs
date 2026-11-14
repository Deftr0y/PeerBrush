//! Held native proposal candidates. Every terminal route transfers or explicitly
//! retires source ownership off the Engine lock; rendering never consumes it.
use crate::{
    engine::{
        native::{self, worker::PreparedNative, NativeDocument},
        scene::SceneSnapshot,
    },
    psd::memory36::Admitted,
    render::native_preview::{self, DisplayOutput, Request, TransientFrame},
    workspace::source_runtime::SourceSession,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
type Result<T> = std::result::Result<T, String>;
mod accept;
mod store;
pub(crate) use store::{
    accept_stored, create_stored, finish_workspace, maintain, maintain_workspace, review_guard,
    review_stored, review_stored_with, Approval, Binding, Store,
};
mod capture;
pub(crate) use capture::CaptureFailure;
#[cfg(test)]
mod tests;

#[derive(Serialize, Deserialize)]
struct Attribution {
    actor: String,
    label: String,
    task: Option<String>,
}
#[must_use = "A proposal must be accepted or explicitly discarded off the Engine lock"]
pub(crate) struct Draft {
    scene: Arc<SceneSnapshot>,
    prepared: PreparedNative,
    attribution: Admitted<Attribution>,
    session: Arc<SourceSession>,
}
#[must_use = "Refused preparation still owns the draft's provisional sources"]
pub(crate) struct Refused {
    pub reason: String,
    draft: Draft,
}
#[must_use = "Failed construction still owns prepared sources and their session"]
pub(crate) struct RejectedPreparation {
    pub reason: String,
    prepared: PreparedNative,
    session: Arc<SourceSession>,
}
impl RejectedPreparation {
    pub(crate) fn prepare_retirement(self) -> std::result::Result<Retirement, (Self, String)> {
        let _activity = self.session.activity();
        let Self {
            reason: original,
            prepared,
            session,
        } = self;
        match prepared.prepare_retirement() {
            Ok(batch) => Ok(Retirement { batch, session }),
            Err((prepared, reason)) => Err((
                Self {
                    reason: original,
                    prepared,
                    session,
                },
                reason,
            )),
        }
    }
    pub(crate) fn into_parts(self) -> (PreparedNative, Arc<SourceSession>) {
        (self.prepared, self.session)
    }
    pub(crate) fn discard(self) -> Result<()> {
        self.prepare_retirement()
            .map_err(|(_, reason)| reason)?
            .retire()
            .map_err(|refused| refused.reason)
    }
}
impl Refused {
    pub(crate) fn into_draft(self) -> Draft {
        self.draft
    }
    pub(crate) fn discard(self) -> Result<()> {
        self.draft.discard()
    }
}
#[must_use = "Publish through the authenticated engine fence or explicitly discard"]
pub(crate) struct Acceptance {
    pending: native::PreparedCommit,
    session: Arc<SourceSession>,
}
#[must_use = "Retire explicitly outside Engine locks; no source I/O runs on Drop"]
pub(crate) struct Retirement {
    batch: native::AbortBatch,
    session: Arc<SourceSession>,
}
#[must_use = "A retryable refusal still owns the original provisional fragments"]
pub(crate) struct RefusedRetirement {
    pub(crate) reason: String,
    retirement: Retirement,
}
impl RefusedRetirement {
    pub(crate) fn into_retirement(self) -> Retirement {
        self.retirement
    }
}
impl Retirement {
    pub(crate) fn retryable(&self) -> bool {
        self.batch.retryable()
    }
    pub(crate) fn retire(self) -> std::result::Result<(), RefusedRetirement> {
        self.retire_with(&|| Ok(()))
    }
    pub(crate) fn retire_with(
        self,
        cleanup: &dyn Fn() -> Result<()>,
    ) -> std::result::Result<(), RefusedRetirement> {
        let Self { batch, session } = self;
        let _activity = session.activity();
        let _foreground = match session.foreground_activity() {
            Ok(foreground) => foreground,
            Err(reason) => {
                return Err(RefusedRetirement {
                    reason,
                    retirement: Self { batch, session },
                });
            }
        };
        match batch.retire(&session, cleanup) {
            Ok(()) => Ok(()),
            Err(refused) => Err(RefusedRetirement {
                reason: refused.reason,
                retirement: Self {
                    batch: refused.batch,
                    session,
                },
            }),
        }
    }
}
impl Acceptance {
    pub(crate) fn prepare_retirement(self) -> std::result::Result<Retirement, (Self, String)> {
        let _activity = self.session.activity();
        let Self { pending, session } = self;
        match pending.prepare_retirement() {
            Ok(batch) => Ok(Retirement { batch, session }),
            Err((pending, reason)) => Err((Self { pending, session }, reason)),
        }
    }
    pub(crate) fn into_parts(self) -> (native::PreparedCommit, Arc<SourceSession>) {
        (self.pending, self.session)
    }
    pub(crate) fn discard(self) -> Result<()> {
        self.prepare_retirement()
            .map_err(|(_, reason)| reason)?
            .retire()
            .map_err(|refused| refused.reason)
    }
}
impl Draft {
    pub(super) fn park_activity(&mut self) -> Result<()> {
        SourceSession::park_activity(&mut self.session)
    }
    /// Transfer out of Engine storage first. This releases candidate views and
    /// returns a source-only owner; verification/unlink remains an explicit step.
    pub(crate) fn prepare_retirement(self) -> std::result::Result<Retirement, Refused> {
        let _activity = self.session.activity();
        let Self {
            scene,
            prepared,
            attribution,
            session,
        } = self;
        match prepared.prepare_retirement() {
            Ok(batch) => {
                drop((scene, attribution));
                Ok(Retirement { batch, session })
            }
            Err((prepared, reason)) => Err(Refused {
                reason,
                draft: Self {
                    scene,
                    prepared,
                    attribution,
                    session,
                },
            }),
        }
    }
    /// Failure returns the original prepared owner for off-lock retirement.
    pub(crate) fn new(
        scene: Arc<SceneSnapshot>,
        prepared: PreparedNative,
        session: Arc<SourceSession>,
        actor: &str,
        label: &str,
        task: Option<&str>,
        check: &dyn Fn() -> Result<()>,
    ) -> std::result::Result<Self, RejectedPreparation> {
        let capture = || -> Result<Admitted<Attribution>> {
            check()?;
            prepared.check()?;
            prepared.check_actor(actor)?;
            session.check()?;
            let base = scene.native_document()?;
            if base.key() != prepared.base_key()
                || !base.budget().same_pool(&session.resident())
                || !base.disk_budget().same_pool(&session.disk())
            {
                return Err("Proposal differs from its captured native source session".into());
            }
            validate_attribution(actor, label, task)?;
            #[derive(Serialize)]
            struct Borrowed<'a> {
                actor: &'a str,
                label: &'a str,
                task: Option<&'a str>,
            }
            native::copy_admitted(&Borrowed { actor, label, task }, base.budget(), true, check)
        };
        match capture() {
            Ok(attribution) => Ok(Self {
                scene,
                prepared,
                attribution,
                session,
            }),
            Err(reason) => Err(RejectedPreparation {
                prepared,
                session,
                reason,
            }),
        }
    }
    pub(crate) fn matches(&self, current: &NativeDocument) -> bool {
        self.prepared.base_key() == current.key()
            && self.attribution.owner().uses_budget(current.budget())
    }
    /// Returns admitted display/source metadata only; no candidate document or
    /// source session escapes the review render.
    pub(crate) fn render(
        &self,
        current: &NativeDocument,
        rect: Option<[i32; 4]>,
        edge: u32,
        target: Option<&str>,
        mask: bool,
        selection_view: Option<&str>,
        check: &dyn Fn() -> Result<()>,
    ) -> Result<TransientFrame> {
        self.render_with(current, rect, edge, target, mask, selection_view, check, false,
            |request, check| request.render(&mut || check()))
    }
    pub(crate) fn render_with<T: DisplayOutput>(
        &self,
        current: &NativeDocument,
        rect: Option<[i32; 4]>,
        edge: u32,
        target: Option<&str>,
        mask: bool,
        selection_view: Option<&str>,
        check: &dyn Fn() -> Result<()>,
        canvas: bool,
        render: impl FnOnce(&Request, &dyn Fn() -> Result<()>) -> Result<T>,
    ) -> Result<TransientFrame<T>> {
        let _activity = self.session.activity();
        if !self.matches(current) {
            return Err("Proposal source changed; review a fresh proposal".into());
        }
        let _foreground = self.session.foreground_activity()?;
        self.prepared.check()?;
        native_preview::render_review_with(
            &self.scene,
            &self.prepared,
            self.session.clone(),
            rect,
            edge,
            target,
            mask,
            selection_view,
            check,
            canvas,
            render,
        )
    }
    /// Human authorization does not change edit attribution. The caller must
    /// still validate the actual live source, task and reservations at publication.
    pub(crate) fn prepare_accept(
        self,
        accepted_by: &str,
        current: &NativeDocument,
        check: &dyn Fn() -> Result<()>,
    ) -> std::result::Result<Acceptance, Refused> {
        let _activity = self.session.activity();
        if accepted_by != "human" || !self.matches(current) {
            return Err(Refused {
                reason: "A current proposal needs explicit human acceptance".into(),
                draft: self,
            });
        }
        let Self {
            scene,
            prepared,
            attribution,
            session,
        } = self;
        let request = native::HistoryRequest {
            actor: &attribution.actor,
            label: &attribution.label,
            task: attribution.task.as_deref(),
            gesture: None,
        };
        let active = || {
            check()?;
            session.check()
        };
        match prepared.prepare_commit(request, &active) {
            Ok(pending) => {
                drop((scene, attribution));
                Ok(Acceptance { pending, session })
            }
            Err((prepared, reason)) => Err(Refused {
                reason,
                draft: Self {
                    scene,
                    prepared,
                    attribution,
                    session,
                },
            }),
        }
    }
    pub(crate) fn discard(self) -> Result<()> {
        self.prepare_retirement()
            .map_err(|refused| refused.reason)?
            .retire()
            .map_err(|refused| refused.reason)
    }
}

fn validate_attribution(actor: &str, label: &str, task: Option<&str>) -> Result<()> {
    if actor.is_empty()
        || actor.len() > 128
        || label.is_empty()
        || label.chars().count() > 160
        || task.is_some_and(|task| task.len() > 256)
    {
        return Err("Invalid native proposal attribution".into());
    }
    Ok(())
}
