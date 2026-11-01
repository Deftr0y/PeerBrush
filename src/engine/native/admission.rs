//! Pure editor capture admission. It grants no filesystem authority.
use super::*;
use crate::{
    engine::{Lease, Scope},
    large_document::Reservation,
};

pub(super) struct EditAdmission {
    actor: String,
    base: DocumentKey,
    scopes: Arc<Intent>,
    descriptor: Option<Arc<CommandDescriptor>>,
    transaction_step: bool,
    _output_charge: Option<Arc<Reservation>>,
    _charge: Reservation,
}
impl NativeDocument {
    /// Call while observing the authoritative Engine and its current write fence.
    /// Only bounded metadata/owner allocation occurs; workers begin source I/O
    /// later through their independently retained SourceSession admission.
    #[cfg(test)]
    pub(super) fn capture_intent(
        &self,
        expected: DocumentKey,
        revision: u64,
        actor: &str,
        scopes: &[Scope],
        leases: &[Lease],
        now: u64,
        closed: bool,
        write_available: bool,
    ) -> Result<CapturedEdit> {
        self.capture_impl(
            expected,
            revision,
            actor,
            scopes,
            leases,
            now,
            closed,
            write_available,
            false,
            None,
        )
    }
    pub(crate) fn capture_descriptor(
        &self,
        descriptor: Arc<CommandDescriptor>,
        actor: &str,
        leases: &[Lease],
        now: u64,
        closed: bool,
        write_available: bool,
    ) -> Result<CapturedEdit> {
        if !descriptor.uses_budget(self.budget()) {
            return Err("Native command descriptor uses another admission".into());
        }
        let mut edit = self.capture_impl(
            descriptor.generation(),
            descriptor.revision(),
            actor,
            descriptor.scopes(),
            leases,
            now,
            closed,
            write_available,
            true,
            Some(descriptor.shared_scopes()),
        )?;
        let admission = edit
            .admission
            .as_mut()
            .and_then(Arc::get_mut)
            .ok_or("Native capture admission unexpectedly shared")?;
        admission.descriptor = Some(descriptor);
        Ok(edit)
    }
    fn capture_impl(
        &self,
        expected: DocumentKey,
        revision: u64,
        actor: &str,
        scopes: &[Scope],
        leases: &[Lease],
        now: u64,
        closed: bool,
        write_available: bool,
        engine_derived: bool,
        shared_scopes: Option<Arc<Intent>>,
    ) -> Result<CapturedEdit> {
        if closed || !write_available {
            return Err("Native editor capture is closed or write-fenced".into());
        }
        if expected != self.key() || revision != self.metadata().revision() {
            return Err("Native editor capture is stale".into());
        }
        if actor.is_empty()
            || actor.len() > 4096
            || scopes.len() > super::super::scene::MAX_RASTER_LOCATIONS
        {
            return Err("Invalid native editor actor or scope count".into());
        }
        for scope in scopes {
            if !engine_derived
                && scope.rect.is_some_and(|r| {
                    r.iter().any(|v| v.unsigned_abs() > 100000) || r[0] > r[2] || r[1] > r[3]
                })
            {
                return Err("Invalid native editor scope rectangle".into());
            }
            if let Some(target) = &scope.target {
                let layer = target.strip_prefix("@visibility:").unwrap_or(target);
                if layer != "@selection"
                    && !self
                        .core
                        .metadata
                        .layers
                        .iter()
                        .any(|item| item.id == layer)
                {
                    return Err("Unknown native editor scope target".into());
                }
            }
        }
        let scopes = match shared_scopes {
            Some(scopes) => scopes,
            None => Arc::new(Intent::from_scopes(
                IntentKind::Capture,
                scopes,
                self.budget(),
            )?),
        };
        scopes.check(self, actor, leases, now)?;
        let bytes = actor
            .len()
            .checked_add(std::mem::size_of::<EditAdmission>() + 128)
            .ok_or("Native actor footprint overflow")?;
        let charge = self
            .budget()
            .reserve_bytes(bytes, bytes)
            .map_err(|e| e.to_string())?;
        let mut owner = String::new();
        owner
            .try_reserve_exact(actor.len())
            .map_err(|_| "Native actor allocation refused")?;
        owner.push_str(actor);
        let mut edit = self.capture_edit()?;
        edit.admission = Some(Arc::new(EditAdmission {
            actor: owner,
            base: self.key(),
            scopes,
            descriptor: None,
            transaction_step: false,
            _output_charge: None,
            _charge: charge,
        }));
        Ok(edit)
    }
}
impl EditAdmission {
    /// Only a captured, generation/epoch-bound task can mint this admission.
    /// The source resolver derived these scopes; no client scope setter exists.
    pub(super) fn task(
        capture: &super::history::TaskCapture,
        scopes: &Intent,
        output_charge: Option<Arc<Reservation>>,
        check: &dyn Fn() -> Result<()>,
    ) -> Result<Arc<Self>> {
        capture.cancellation().check()?;
        check()?;
        let base = capture.current();
        let bytes = capture
            .actor()
            .len()
            .checked_add(std::mem::size_of::<Self>() + 128)
            .ok_or("Native task admission footprint overflow")?;
        let charge = base
            .budget()
            .reserve_bytes(bytes, bytes)
            .map_err(|e| e.to_string())?;
        let mut actor = String::new();
        actor
            .try_reserve_exact(capture.actor().len())
            .map_err(|_| "Native task actor allocation refused")?;
        actor.push_str(capture.actor());
        let scopes = Arc::new(Intent::from_scopes(
            IntentKind::History,
            scopes.scopes(),
            base.budget(),
        )?);
        Ok(Arc::new(Self {
            actor,
            base: base.key(),
            scopes,
            descriptor: None,
            transaction_step: false,
            _output_charge: output_charge,
            _charge: charge,
        }))
    }
    pub(super) fn is_transaction_step(&self) -> bool {
        self.transaction_step
    }
    pub(super) fn check_publication(
        &self,
        base: &NativeDocument,
        actor: &str,
        intent: &Intent,
    ) -> Result<()> {
        if self.transaction_step {
            return Err("Native transaction steps cannot publish independently".into());
        }
        self.check(base, actor, intent)
    }
    pub(super) fn shared_descriptor(&self) -> Option<Arc<CommandDescriptor>> {
        self.descriptor.clone()
    }
    pub(super) fn actor(&self) -> &str {
        &self.actor
    }
    pub(super) fn with_scopes(
        &self,
        base: &NativeDocument,
        scopes: Arc<Intent>,
        output_charge: Arc<Reservation>,
        transaction_step: bool,
    ) -> Result<Arc<Self>> {
        let bytes = self
            .actor
            .len()
            .checked_add(std::mem::size_of::<Self>() + 128)
            .ok_or("Native transaction admission footprint overflow")?;
        let charge = base
            .budget()
            .reserve_bytes(bytes, bytes)
            .map_err(|e| e.to_string())?;
        let mut actor = String::new();
        actor
            .try_reserve_exact(self.actor.len())
            .map_err(|_| "Native transaction actor allocation refused")?;
        actor.push_str(&self.actor);
        Ok(Arc::new(Self {
            actor,
            base: base.key(),
            scopes,
            descriptor: self.descriptor.clone(),
            transaction_step,
            _output_charge: Some(output_charge),
            _charge: charge,
        }))
    }
    pub(super) fn descriptor(&self) -> Option<&CommandDescriptor> {
        self.descriptor.as_deref()
    }
    pub(super) fn planned_intent(&self, base: &NativeDocument, intent: Intent) -> Result<Intent> {
        self.check(base, &self.actor, &intent)?;
        if self.descriptor.is_some() {
            Intent::from_scopes(*intent.kind(), self.scopes.scopes(), base.budget())
        } else {
            Ok(intent)
        }
    }
    pub(super) fn check(&self, base: &NativeDocument, actor: &str, intent: &Intent) -> Result<()> {
        if self.base != base.key() || self.actor != actor {
            return Err("Native editor admission belongs to another actor or generation".into());
        }
        for actual in intent.scopes() {
            let allowed = self.scopes.scopes().iter().any(|requested| {
                let Some(target) = requested.target.as_deref() else {
                    return true;
                };
                let Some(actual) = actual.target.as_deref() else {
                    return false;
                };
                if target == actual {
                    return true;
                }
                if target.starts_with('@') || actual.starts_with('@') {
                    return false;
                }
                let mut current = Some(actual);
                for _ in 0..=16 {
                    let Some(id) = current else {
                        return false;
                    };
                    if id == target {
                        return true;
                    }
                    current = base
                        .core
                        .metadata
                        .layers
                        .iter()
                        .find(|layer| layer.id == id)
                        .and_then(|layer| layer.parent.as_deref());
                }
                false
            });
            if !allowed {
                return Err("Prepared native edit exceeds its captured targets".into());
            }
        }
        Ok(())
    }
}
impl CapturedEdit {
    pub(super) fn into_transaction_step(mut self) -> Result<Self> {
        let admission = self
            .admission
            .as_mut()
            .and_then(Arc::get_mut)
            .ok_or("Native step admission unexpectedly shared")?;
        admission.transaction_step = true;
        Ok(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn capture(
        doc: &NativeDocument,
        actor: &str,
        scopes: &[Scope],
        leases: &[Lease],
    ) -> Result<CapturedEdit> {
        doc.capture_intent(
            doc.key(),
            doc.metadata().revision(),
            actor,
            scopes,
            leases,
            1,
            false,
            true,
        )
    }
    #[test]
    fn current_reservations_are_checked_before_worker_source_admission() {
        let pool = ResidentBudget::new(8 * 1024 * 1024).unwrap();
        let doc = super::super::tests::document(&pool);
        let layer = doc.metadata().layer(0).unwrap().id();
        let scopes = [Scope::layer(layer)];
        let lease = Lease {
            id: "lease".into(),
            owner: "other".into(),
            description: "paint".into(),
            scopes: vec![Scope::layer(layer)],
            expires: 100,
        };
        let baseline = pool.used();
        assert!(capture(&doc, "worker", &scopes, &[lease])
            .err()
            .unwrap()
            .contains("Reserved"));
        assert_eq!(pool.used(), baseline);
        assert_eq!(doc.disk_budget().used().unwrap(), 0);
    }
    #[test]
    fn stale_closed_fenced_and_invalid_capture_leave_owner_accounting_unchanged() {
        let pool = ResidentBudget::new(8 * 1024 * 1024).unwrap();
        let doc = super::super::tests::document(&pool);
        let scopes = [Scope {
            target: None,
            rect: None,
        }];
        let baseline = pool.used();
        assert!(doc
            .capture_intent(doc.key(), 1, "worker", &scopes, &[], 1, false, true)
            .is_err());
        assert!(doc
            .capture_intent(key(), 0, "worker", &scopes, &[], 1, false, true)
            .is_err());
        assert!(doc
            .capture_intent(doc.key(), 0, "worker", &scopes, &[], 1, true, true)
            .is_err());
        assert!(doc
            .capture_intent(doc.key(), 0, "worker", &scopes, &[], 1, false, false)
            .is_err());
        assert!(capture(&doc, "worker", &[Scope::layer("missing")], &[]).is_err());
        assert!(capture(
            &doc,
            "worker",
            &[Scope {
                target: None,
                rect: Some([2, 0, 1, 1])
            }],
            &[]
        )
        .is_err());
        assert_eq!(pool.used(), baseline);
        assert_eq!(doc.metadata().revision(), 0);
    }
    #[test]
    fn admitted_actor_and_targets_are_rechecked_before_history_preparation_and_publication() {
        let pool = ResidentBudget::new(8 * 1024 * 1024).unwrap();
        let mut doc = super::super::tests::document(&pool);
        let mut history = NativeHistory::new(&pool, 2).unwrap();
        let layer = doc.metadata().layer(0).unwrap().id().to_string();
        let captured = capture(&doc, "worker", &[Scope::layer("@selection")], &[]).unwrap();
        let refused = captured
            .visibility(&layer, false, &|| Ok(()))
            .err()
            .unwrap();
        assert!(refused.contains("captured targets"));
        let captured = capture(
            &doc,
            "worker",
            &[Scope {
                target: None,
                rect: None,
            }],
            &[],
        )
        .unwrap();
        let delta = captured.visibility(&layer, false, &|| Ok(())).unwrap();
        let refused = doc.publish("other", &[], 1, false, delta).unwrap_err();
        assert!(refused.reason().contains("another actor"));
        drop(refused);
        let delta = doc
            .capture_edit()
            .unwrap()
            .visibility(&layer, false, &|| Ok(()))
            .unwrap();
        let refused = history
            .snapshot()
            .prepare_commit(
                &doc,
                delta,
                HistoryRequest {
                    actor: "worker",
                    label: "edit",
                    task: None,
                    gesture: None,
                },
                &|| Ok(()),
            )
            .err()
            .unwrap();
        assert!(refused.reason().contains("admission is missing"));
        drop(refused);
        let captured = capture(
            &doc,
            "worker",
            &[Scope {
                target: None,
                rect: None,
            }],
            &[],
        )
        .unwrap();
        let delta = captured.visibility(&layer, false, &|| Ok(())).unwrap();
        let prepared = history
            .snapshot()
            .prepare_commit(
                &doc,
                delta,
                HistoryRequest {
                    actor: "worker",
                    label: "edit",
                    task: None,
                    gesture: None,
                },
                &|| Ok(()),
            )
            .unwrap_or_else(|e| panic!("{}", e.reason()));
        drop(
            history
                .commit(
                    &mut doc,
                    Publication {
                        actor: "worker",
                        leases: &[],
                        now: 1,
                        closed: false,
                        write_available: true,
                    },
                    prepared,
                )
                .unwrap(),
        );
        assert_eq!(doc.metadata().revision(), 1);
        assert_eq!(history.undo_len(), 1);
        drop(history);
        drop(doc);
        assert_eq!(pool.used(), 0);
    }
}
