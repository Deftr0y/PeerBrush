//! Immutable runtime payloads and pre-admitted publication presentation.
use super::{authority::NativeSeed, native, AiChange, Engine, Scope};
use crate::{large_document::ResidentBudget, psd::memory36::MetadataCharge};
use serde::{Serialize, Serializer};
use std::{ops::Deref, sync::Arc};
type Result<T> = std::result::Result<T, String>;

pub struct RuntimeValue<T>(Arc<Owned<T>>);
struct Owned<T> {
    value: T,
    charge: Option<Arc<MetadataCharge>>,
}
impl<T> Clone for RuntimeValue<T> {
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}
impl<T> From<T> for RuntimeValue<T> {
    fn from(value: T) -> Self {
        Self(Arc::new(Owned {
            value,
            charge: None,
        }))
    }
}
impl From<&str> for RuntimeValue<String> {
    fn from(value: &str) -> Self {
        value.to_owned().into()
    }
}
impl<T> Deref for RuntimeValue<T> {
    type Target = T;
    fn deref(&self) -> &T {
        &self.0.value
    }
}
impl<T: std::fmt::Display> std::fmt::Display for RuntimeValue<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(&self.0.value, f)
    }
}
impl<T> AsRef<T> for RuntimeValue<T> {
    fn as_ref(&self) -> &T {
        &self.0.value
    }
}
impl<T: Serialize> Serialize for RuntimeValue<T> {
    fn serialize<S: Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        self.0.value.serialize(s)
    }
}
impl<T: std::fmt::Debug> std::fmt::Debug for RuntimeValue<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.value.fmt(f)
    }
}
impl<T: PartialEq> PartialEq for RuntimeValue<T> {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0) || self.0.value == other.0.value
    }
}
impl PartialEq<&str> for RuntimeValue<String> {
    fn eq(&self, other: &&str) -> bool {
        self.as_str() == *other
    }
}
impl<T> RuntimeValue<T> {
    fn admitted(value: T, charge: Arc<MetadataCharge>) -> Self {
        Self(Arc::new(Owned {
            value,
            charge: Some(charge),
        }))
    }
    fn uses_budget(&self, pool: &ResidentBudget) -> bool {
        self.0.charge.as_ref().is_some_and(|c| c.uses_budget(pool))
    }
}

pub(crate) struct PreparedEngineEffects {
    record: native::DocumentKey,
    base: native::DocumentKey,
    serial: u64,
    status: RuntimeValue<String>,
    ai: Option<RuntimeValue<AiChange>>,
}
pub(crate) struct RetiredEngineCommit {
    history: native::RetiredHistory,
    _status: RuntimeValue<String>,
    _ai: Option<RuntimeValue<AiChange>>,
}
pub(crate) struct RefusedEngineCommit {
    refused: native::RefusedCommit,
    effects: PreparedEngineEffects,
}
impl RefusedEngineCommit {
    pub(crate) fn reason(&self) -> &str {
        self.refused.reason()
    }
    pub(crate) fn abort(
        self,
        session: &impl super::scene::StageSession,
        cleanup: &dyn Fn() -> Result<()>,
    ) -> Result<()> {
        let Self { refused, effects } = self;
        drop(effects);
        refused.abort(session, cleanup)
    }
}
impl RetiredEngineCommit {
    pub(crate) fn retire(
        self,
        session: &impl super::scene::StageSession,
        cleanup: &dyn Fn() -> Result<()>,
    ) -> Result<()> {
        let Self {
            history,
            _status,
            _ai,
        } = self;
        drop((_status, _ai));
        history.retire(session, cleanup)
    }
}
fn text(value: &str) -> Result<String> {
    let mut out = String::new();
    out.try_reserve_exact(value.len())
        .map_err(|_| "Runtime text allocation refused")?;
    if out.capacity() > value.len() {
        return Err("Runtime text exceeds admission".into());
    }
    out.push_str(value);
    Ok(out)
}
fn visuals(
    view: &native::CommitRuntime<'_>,
    base: &native::NativeDocument,
    visit: &mut dyn FnMut(Option<&str>, Option<[i32; 4]>) -> Result<()>,
) -> Result<usize> {
    let mut count = 0;
    if let Some(descriptor) = view.descriptor {
        for command in descriptor.commands() {
            if let Some(target) = command.parameters()["layer"].as_str() {
                visit(
                    Some(target),
                    command
                        .parameters()
                        .get("rect")
                        .and_then(super::rect)
                        .or_else(|| command.write_scopes().first().and_then(|s| s.rect)),
                )?;
                count += 1;
            }
        }
        for layer in view.candidate.metadata().layers() {
            if !base.metadata().layers().any(|old| old.id() == layer.id()) {
                visit(Some(layer.id()), None)?;
                count += 1;
            }
        }
    }
    if count == 0 {
        for scope in view.scopes {
            visit(scope.target.as_deref(), scope.rect)?;
            count += 1;
        }
    }
    Ok(count)
}
impl PreparedEngineEffects {
    /// All payload copies/allocations precede the final Engine publication fence.
    /// Actor, label, command and scopes come from the actual prepared history.
    pub(crate) fn prepare(
        seed: &NativeSeed,
        prepared: &native::PreparedCommit,
        check: &dyn Fn() -> Result<()>,
    ) -> Result<Self> {
        check()?;
        let view = prepared.runtime_view();
        let base = seed.document();
        if view.base != base.key()
            || view.candidate.metadata().id() != base.metadata().id()
            || base.metadata().revision().checked_add(1)
                != Some(view.candidate.metadata().revision())
        {
            return Err("Runtime effects belong to another prepared document".into());
        }
        let status_charge = MetadataCharge::reserve(
            base.budget(),
            view.label
                .len()
                .checked_add(512)
                .ok_or("Runtime status footprint overflow")?,
            check,
        )?;
        let status = RuntimeValue::admitted(text(view.label)?, status_charge);
        let mut ai = None;
        if view.actor != "human" {
            let serial = seed
                .runtime_serial()
                .checked_add(1)
                .ok_or("Runtime activity serial overflow")?;
            let tool = view
                .descriptor
                .and_then(|d| d.commands().last())
                .map_or("history", |c| super::activity_tool(c.parameters()));
            let mut bytes = std::mem::size_of::<AiChange>()
                .checked_add(512)
                .and_then(|n| n.checked_add(view.actor.len()))
                .and_then(|n| n.checked_add(view.label.len()))
                .and_then(|n| n.checked_add(tool.len()))
                .ok_or("Runtime activity footprint overflow")?;
            let count = visuals(&view, base, &mut |target, _| {
                check()?;
                bytes = bytes
                    .checked_add(std::mem::size_of::<Scope>())
                    .and_then(|n| n.checked_add(target.map_or(0, str::len)))
                    .ok_or("Runtime scope footprint overflow")?;
                Ok(())
            })?;
            let charge = MetadataCharge::reserve(base.budget(), bytes, check)?;
            let mut scopes = Vec::new();
            scopes
                .try_reserve_exact(count)
                .map_err(|_| "Runtime scope allocation refused")?;
            if scopes.capacity() > count {
                return Err("Runtime scopes exceed admission".into());
            }
            visuals(&view, base, &mut |target, rect| {
                check()?;
                scopes.push(Scope {
                    target: target.map(text).transpose()?,
                    rect,
                });
                Ok(())
            })?;
            ai = Some(RuntimeValue::admitted(
                AiChange {
                    serial,
                    actor: text(view.actor)?,
                    label: text(view.label)?,
                    tool: text(tool)?,
                    scopes,
                    at: 0,
                },
                charge,
            ));
        }
        check()?;
        Ok(Self {
            record: view.record,
            base: view.base,
            serial: seed.runtime_serial(),
            status,
            ai,
        })
    }
}
impl Engine {
    pub(crate) fn commit_native_with_effects(
        &mut self,
        seed: &NativeSeed,
        actor: &str,
        prepared: native::PreparedCommit,
        mut effects: PreparedEngineEffects,
    ) -> std::result::Result<RetiredEngineCommit, RefusedEngineCommit> {
        let view = prepared.runtime_view();
        let valid = effects.record == view.record
            && effects.base == view.base
            && effects.base == seed.document().key()
            && effects.serial == self.ai_serial
            && actor == view.actor
            && effects.status.uses_budget(seed.document().budget())
            && effects
                .ai
                .as_ref()
                .is_none_or(|a| a.uses_budget(seed.document().budget()));
        if !valid {
            return Err(RefusedEngineCommit {
                refused: prepared.refused("Runtime effects differ from prepared publication"),
                effects,
            });
        }
        let at = super::now();
        if let Some(ai) = &mut effects.ai {
            // This owner is private and unpublished. Finalize its small header
            // before publication; shared or escaped owners fail closed.
            let Some(owned) = Arc::get_mut(&mut ai.0) else {
                return Err(RefusedEngineCommit {
                    refused: prepared.refused("Runtime activity was shared before publication"),
                    effects,
                });
            };
            owned.value.at = at;
        }
        let history = match self.commit_native(seed, actor, prepared) {
            Ok(history) => history,
            Err(refused) => return Err(RefusedEngineCommit { refused, effects }),
        };
        for lease in &mut self.leases {
            if lease.owner == actor {
                lease.expires = at.saturating_add(300);
            }
        }
        let old_status = std::mem::replace(&mut self.status, effects.status);
        let old_ai = if let Some(ai) = effects.ai {
            self.ai_serial = ai.serial;
            self.ai_change.replace(ai)
        } else {
            None
        };
        Ok(RetiredEngineCommit {
            history,
            _status: old_status,
            _ai: old_ai,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn runtime_clones_share_payload_and_charge_with_exact_wire() {
        let pool = ResidentBudget::new(1024 * 1024).unwrap();
        let charge = MetadataCharge::reserve(&pool, 4096, &|| Ok(())).unwrap();
        let value = RuntimeValue::admitted(String::from("actual status"), charge);
        let copy = value.clone();
        assert_eq!(serde_json::to_string(&value).unwrap(), "\"actual status\"");
        assert_eq!(value.as_ptr(), copy.as_ptr());
        assert_eq!(pool.used(), 4096);
        drop(value);
        assert_eq!(pool.used(), 4096);
        drop(copy);
        assert_eq!(pool.used(), 0);
        let legacy: RuntimeValue<String> = "legacy wire".into();
        assert_eq!(serde_json::to_string(&legacy).unwrap(), "\"legacy wire\"");
        assert_eq!(pool.used(), 0);
    }
    #[test]
    fn prepared_effects_take_actor_scopes_from_history_and_keep_refused_payloads_owned() {
        use super::super::authority::DocumentAuthority;
        let pool = ResidentBudget::new(8 * 1024 * 1024).unwrap();
        let disk = crate::large_document::DiskBudget::new(1024 * 1024, 4).unwrap();
        let source = native::NativeDocument::new_blank(4, 2, 16, &pool, &disk, &|| Ok(())).unwrap();
        let state = DocumentAuthority::from_native(source.clone(), 8).unwrap();
        // This packet's live native facade remains closed until its coordinated
        // field switch. The fixture exercises factory data and retained refusal,
        // and grants no native publication to the resident Engine below.
        let mut engine = Engine::new();
        let seed = NativeSeed::capture(&engine, &source).unwrap();
        let layer = source.metadata().layer(0).unwrap().id().to_owned();
        let descriptor = native::CommandDescriptor::from_normalized(
            &source,
            0,
            &[serde_json::json!({"op":"layer.update","layer":layer,"visible":false})],
            &|| Ok(()),
        )
        .unwrap();
        let capture = state
            .capture_native(descriptor, "agent", &[], 0, false, true)
            .unwrap();
        let (document, history, edit) = capture.into_parts();
        let delta = edit.visibility(&layer, false, &|| Ok(())).unwrap();
        let prepared = history
            .prepare_commit(
                &document,
                delta,
                native::HistoryRequest {
                    actor: "agent",
                    label: "AI visibility",
                    task: None,
                    gesture: None,
                },
                &|| Ok(()),
            )
            .unwrap();
        let baseline = pool.used();
        assert!(PreparedEngineEffects::prepare(&seed, &prepared, &|| Err(
            "cancelled effects".into()
        ))
        .is_err());
        assert_eq!(pool.used(), baseline);
        let remainder = pool.limit() - baseline - 1;
        let ballast = pool.reserve_bytes(remainder, remainder).unwrap();
        let occupied = pool.used();
        assert!(PreparedEngineEffects::prepare(&seed, &prepared, &|| Ok(())).is_err());
        assert_eq!(pool.used(), occupied);
        drop(ballast);
        let effects = PreparedEngineEffects::prepare(&seed, &prepared, &|| Ok(())).unwrap();
        assert_eq!(effects.status.as_str(), "AI visibility");
        let ai = effects.ai.as_ref().unwrap();
        assert_eq!(ai.actor, "agent");
        assert_eq!(ai.label, "AI visibility");
        assert_eq!(ai.serial, 1);
        assert_eq!(ai.tool, "layers");
        assert_eq!(ai.scopes.len(), 1);
        assert_eq!(ai.scopes[0].target.as_deref(), Some(layer.as_str()));
        let used = pool.used();
        let observer = ai.clone();
        assert_eq!(pool.used(), used);
        let refused = match engine.commit_native_with_effects(&seed, "agent", prepared, effects) {
            Err(r) => r,
            Ok(_) => panic!("shared unpublished activity was accepted"),
        };
        assert_eq!(
            refused.reason(),
            "Runtime activity was shared before publication"
        );
        assert_eq!(engine.history_lengths(), [0, 0]);
        drop(refused);
        assert_eq!(observer.actor, "agent");
        drop((observer, history, document, seed, source, state, engine));
        assert_eq!(pool.used(), 0);
    }
}
