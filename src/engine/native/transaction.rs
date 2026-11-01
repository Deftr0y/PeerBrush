//! Private working generations for one normalized, atomic command batch.
//! No step publishes blocks, advances public revision, or enters history.
use super::*;
use crate::large_document::Reservation;

pub(crate) struct NativeTransaction {
    captured: CapturedEdit,
    working: NativeDocument,
    descriptor: Arc<CommandDescriptor>,
    completed: usize,
    scopes: Arc<Intent>,
    outputs: Vec<StagedOutput>,
    _output_charge: Arc<Reservation>,
}
#[must_use]
pub(crate) struct RefusedTransaction {
    reason: String,
    transaction: NativeTransaction,
}
impl std::fmt::Debug for RefusedTransaction {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("RefusedTransaction")
            .field(&self.reason)
            .finish()
    }
}
impl RefusedTransaction {
    pub fn reason(&self) -> &str {
        &self.reason
    }
    pub fn into_transaction(self) -> NativeTransaction {
        self.transaction
    }
    pub fn abort(
        self,
        session: &impl super::super::scene::StageSession,
        cleanup: &dyn Fn() -> Result<()>,
    ) -> Result<()> {
        self.transaction.abort(session, cleanup)
    }
}
impl CapturedEdit {
    pub(crate) fn transaction(self) -> Result<NativeTransaction> {
        self.cancellation.check()?;
        let descriptor = self
            .admission
            .as_ref()
            .and_then(|a| a.shared_descriptor())
            .ok_or("Native transaction requires a normalized command capture")?;
        if descriptor.generation() != self.base.key() {
            return Err("Native transaction descriptor is stale".into());
        }
        let charge = self
            .base
            .budget()
            .reserve_bytes(128, 128)
            .map_err(|e| e.to_string())?;
        Ok(NativeTransaction {
            working: self.base.clone(),
            scopes: descriptor.shared_scopes(),
            captured: self,
            descriptor,
            completed: 0,
            outputs: Vec::new(),
            _output_charge: Arc::new(charge),
        })
    }
}
impl NativeTransaction {
    pub fn working(&self) -> &NativeDocument {
        &self.working
    }
    pub fn completed(&self) -> usize {
        self.completed
    }
    /// Must run outside Engine locks. The handler reads only this capture's
    /// command view; it receives no second independently mutable parameter list.
    pub fn next(&self, check: &dyn Fn() -> Result<()>) -> Result<CapturedEdit> {
        self.captured.cancellation.check()?;
        check()?;
        let descriptor = self.descriptor.step(&self.working, self.completed, check)?;
        let actor = self
            .captured
            .admission
            .as_ref()
            .ok_or("Missing native capture admission")?
            .actor();
        let mut next = self
            .working
            .capture_descriptor(descriptor, actor, &[], 0, false, true)?;
        next.cancellation = self.captured.cancellation.clone();
        Ok(next)
    }
    /// All checks/allocation precede the swap. Refusal returns the exact staged
    /// output tokens so the caller can retire them using its retained session.
    pub fn complete(
        &mut self,
        delta: PreparedDelta,
        check: &dyn Fn() -> Result<()>,
    ) -> std::result::Result<(), RefusedDelta> {
        let prepared = (|| {
            self.captured.cancellation.check()?;
            check()?;
            let expected = self
                .descriptor
                .commands()
                .nth(self.completed)
                .ok_or("Native transaction has no remaining command")?
                .token();
            let admission = delta
                .admission
                .as_ref()
                .ok_or("Native transaction step is unadmitted")?;
            let descriptor = admission
                .descriptor()
                .ok_or("Native transaction step has no descriptor")?;
            if descriptor.commands().len() != 1
                || descriptor.commands().next().unwrap().token() != expected
                || descriptor.generation() != self.working.key()
                || delta.base != self.working.key()
                || !Arc::ptr_eq(&delta.cancellation.0, &self.captured.cancellation.0)
                || delta.candidate.metadata().id() != self.working.metadata().id()
                || self.working.metadata().revision().checked_add(1)
                    != Some(delta.candidate.metadata().revision())
            {
                return Err("Native transaction step is foreign, out of order, or stale".into());
            }
            admission.check(
                &self.working,
                self.captured.admission.as_ref().unwrap().actor(),
                &delta.intent,
            )?;
            // Every step observes the batch's original source_revision, matching
            // Engine.batch. The final candidate alone advances that revision.
            let working = restamp(&delta.candidate, self.descriptor.revision(), check)?;
            let scopes = Arc::new(self.scopes.combined(&delta.intent, self.working.budget())?);
            let count = self
                .outputs
                .len()
                .checked_add(delta.outputs.len())
                .ok_or("Native transaction output overflow")?;
            let bytes = count
                .checked_mul(std::mem::size_of::<StagedOutput>())
                .and_then(|n| n.checked_add(128))
                .ok_or("Native transaction output footprint overflow")?;
            let charge = self
                .working
                .budget()
                .reserve_bytes(bytes, bytes)
                .map_err(|e| e.to_string())?;
            self.captured.cancellation.check()?;
            check()?;
            let mut outputs = Vec::new();
            outputs
                .try_reserve_exact(count)
                .map_err(|_| "Native transaction output allocation refused")?;
            if outputs.capacity() > count {
                return Err("Native transaction output allocation exceeds admission".into());
            }
            Ok((working, scopes, outputs, Arc::new(charge)))
        })();
        let (working, scopes, mut outputs, charge) = match prepared {
            Ok(prepared) => prepared,
            Err(reason) => {
                return Err(RefusedDelta {
                    reason,
                    prepared: delta,
                })
            }
        };
        outputs.append(&mut self.outputs);
        outputs.extend(delta.outputs);
        self.outputs = outputs;
        self._output_charge = charge;
        self.working = working;
        self.scopes = scopes;
        self.completed += 1;
        Ok(())
    }
    pub fn finish(
        self,
        check: &dyn Fn() -> Result<()>,
    ) -> std::result::Result<PreparedDelta, RefusedTransaction> {
        let prepared = (|| {
            self.captured.cancellation.check()?;
            check()?;
            if self.completed != self.descriptor.commands().len() {
                return Err("Native transaction is incomplete".into());
            }
            let revision = self
                .descriptor
                .revision()
                .checked_add(1)
                .ok_or("Native revision overflow")?;
            let candidate = restamp(&self.working, revision, check)?;
            let admission = self
                .captured
                .admission
                .as_ref()
                .ok_or("Missing native transaction admission")?
                .with_scopes(
                    &self.captured.base,
                    self.scopes.clone(),
                    self._output_charge.clone(),
                )?;
            let intent = Intent::from_scopes(
                IntentKind::Capture,
                self.scopes.scopes(),
                self.working.budget(),
            )?;
            self.captured.cancellation.check()?;
            check()?;
            Ok((candidate, admission, intent))
        })();
        let (candidate, admission, intent) = match prepared {
            Ok(prepared) => prepared,
            Err(reason) => {
                return Err(RefusedTransaction {
                    reason,
                    transaction: self,
                })
            }
        };
        Ok(PreparedDelta {
            base: self.captured.base.key(),
            candidate,
            cancellation: self.captured.cancellation,
            intent,
            outputs: self.outputs,
            admission: Some(admission),
        })
    }
    pub fn abort(
        self,
        session: &impl super::super::scene::StageSession,
        cleanup: &dyn Fn() -> Result<()>,
    ) -> Result<()> {
        let Self {
            working,
            outputs,
            _output_charge,
            ..
        } = self;
        drop(working);
        let result = super::output::abort_outputs(outputs, session, cleanup);
        drop(_output_charge);
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn begin(base: &NativeDocument, commands: &[serde_json::Value]) -> NativeTransaction {
        let descriptor =
            CommandDescriptor::from_normalized(base, base.metadata().revision(), commands, &|| {
                Ok(())
            })
            .unwrap();
        base.capture_descriptor(descriptor, "worker", &[], 1, false, true)
            .unwrap()
            .transaction()
            .unwrap()
    }
    fn select(edit: CapturedEdit, bounds: [i32; 4]) -> PreparedDelta {
        edit.batch(
            RasterBatch {
                rasters: vec![],
                layers: vec![],
                effects: vec![],
                selection: Some(SelectionPatch {
                    bounds: Some(bounds),
                    polygon: None,
                    current: None,
                    previous: None,
                }),
            },
            &|| Ok(()),
        )
        .unwrap_or_else(|e| panic!("{}", e.reason()))
    }
    #[test]
    fn hundred_commands_advance_once_and_one_history_record_preserves_original() {
        let pool = ResidentBudget::new(16 * 1024 * 1024).unwrap();
        let mut base = super::super::tests::document(&pool);
        let layer = base.metadata().layer(0).unwrap().id().to_owned();
        let commands = (0..100)
            .map(|i| json!({"op":"layer.update","layer":layer,"visible":i%2!=0}))
            .collect::<Vec<_>>();
        let original = base.key();
        let mut transaction = begin(&base, &commands);
        for i in 0..100 {
            let step = transaction.next(&|| Ok(())).unwrap();
            assert_eq!(step.document().metadata().revision(), 0);
            let prepared = step.visibility(&layer, i % 2 != 0, &|| Ok(())).unwrap();
            transaction.complete(prepared, &|| Ok(())).unwrap();
            assert_eq!(transaction.completed(), i + 1);
            assert_eq!(base.key(), original);
        }
        assert!(transaction.next(&|| Ok(())).is_err());
        let prepared = transaction.finish(&|| Ok(())).unwrap();
        assert_eq!(prepared.preview().metadata().revision(), 1);
        let mut history = NativeHistory::new(&pool, 20).unwrap();
        let commit = history
            .snapshot()
            .prepare_commit(
                &base,
                prepared,
                HistoryRequest {
                    actor: "worker",
                    label: "batch",
                    task: Some("task"),
                    gesture: None,
                },
                &|| Ok(()),
            )
            .unwrap();
        drop(
            history
                .commit(
                    &mut base,
                    Publication {
                        actor: "worker",
                        leases: &[],
                        now: 1,
                        closed: false,
                        write_available: true,
                    },
                    commit,
                )
                .unwrap(),
        );
        assert_eq!(history.undo_len(), 1);
        assert_eq!(base.metadata().revision(), 1);
        assert!(base.metadata().layer(0).unwrap().visible());
        let undo = history
            .snapshot()
            .prepare_move(&base, Direction::Undo, "worker", &|| Ok(()))
            .unwrap();
        drop(
            history
                .move_cursor(
                    &mut base,
                    Publication {
                        actor: "worker",
                        leases: &[],
                        now: 1,
                        closed: false,
                        write_available: true,
                    },
                    undo,
                )
                .unwrap(),
        );
        assert_eq!(base.metadata().revision(), 2);
        assert!(base.metadata().layer(0).unwrap().visible());
        drop(base);
        drop(history);
        assert_eq!(pool.used(), 0);
    }
    #[test]
    fn selection_changes_replan_the_next_write_and_keep_original_source_revision() {
        let pool = ResidentBudget::new(16 * 1024 * 1024).unwrap();
        let base = super::super::tests::document(&pool);
        let layer = base.metadata().layer(0).unwrap().id().to_owned();
        let mut transaction = begin(
            &base,
            &[
                json!({"op":"selection","rect":[4,4,8,8]}),
                json!({"op":"paint","layer":layer,"points":[[6.,6.]],"radius":2.,"source_revision":0}),
            ],
        );
        let first = select(transaction.next(&|| Ok(())).unwrap(), [4, 4, 8, 8]);
        transaction.complete(first, &|| Ok(())).unwrap();
        let step = transaction.next(&|| Ok(())).unwrap();
        let command = step.descriptor().unwrap().commands().next().unwrap();
        assert_eq!(command.parameters()["source_revision"], 0);
        assert_eq!(command.write_scopes()[0].rect, Some([4, 4, 8, 8]));
        command
            .check_write(
                step.document(),
                step.document().layer_slot(&layer).unwrap(),
                [4, 4, 8, 8],
            )
            .unwrap();
        assert!(command
            .check_write(
                step.document(),
                step.document().layer_slot(&layer).unwrap(),
                [0, 0, 1, 1]
            )
            .is_err());
    }
    #[test]
    fn incomplete_foreign_out_of_order_cancelled_and_stale_batches_refuse_atomically() {
        let pool = ResidentBudget::new(16 * 1024 * 1024).unwrap();
        let base = super::super::tests::document(&pool);
        let layer = base.metadata().layer(0).unwrap().id().to_owned();
        let commands = [
            json!({"op":"layer.update","layer":layer,"visible":false}),
            json!({"op":"layer.update","layer":layer,"visible":true}),
        ];
        let mut transaction = begin(&base, &commands);
        let original = base.key();
        let one = transaction
            .next(&|| Ok(()))
            .unwrap()
            .visibility(&layer, false, &|| Ok(()))
            .unwrap();
        let duplicate = transaction
            .next(&|| Ok(()))
            .unwrap()
            .visibility(&layer, false, &|| Ok(()))
            .unwrap();
        let mut foreign = begin(&base, &commands);
        assert!(foreign.complete(duplicate, &|| Ok(())).is_err());
        transaction.complete(one, &|| Ok(())).unwrap();
        let stale = transaction.next(&|| Ok(())).unwrap();
        let cancellation = stale.cancellation();
        cancellation.cancel();
        assert!(stale.visibility(&layer, true, &|| Ok(())).is_err());
        assert!(transaction.next(&|| Ok(())).is_err());
        assert!(transaction.finish(&|| Ok(())).is_err());
        assert_eq!(base.key(), original);
        assert!(begin(&base, &commands).finish(&|| Ok(())).is_err());
    }
    #[test]
    fn late_lease_and_changed_generation_still_refuse_the_final_batch() {
        let pool = ResidentBudget::new(16 * 1024 * 1024).unwrap();
        let base = super::super::tests::document(&pool);
        let layer = base.metadata().layer(0).unwrap().id().to_owned();
        let command = [json!({"op":"layer.update","layer":layer,"visible":false})];
        let mut transaction = begin(&base, &command);
        let first = transaction
            .next(&|| Ok(()))
            .unwrap()
            .visibility(&layer, false, &|| Ok(()))
            .unwrap();
        transaction.complete(first, &|| Ok(())).unwrap();
        let prepared = transaction.finish(&|| Ok(())).unwrap();
        let lease = crate::engine::Lease {
            id: "late".into(),
            owner: "other".into(),
            description: "visible".into(),
            scopes: vec![crate::engine::Scope::layer(&format!("@visibility:{layer}"))],
            expires: 100,
        };
        assert!(base
            .publish("worker", &[lease], 1, false, prepared)
            .is_err());
        let mut transaction = begin(&base, &command);
        let first = transaction
            .next(&|| Ok(()))
            .unwrap()
            .visibility(&layer, false, &|| Ok(()))
            .unwrap();
        transaction.complete(first, &|| Ok(())).unwrap();
        let prepared = transaction.finish(&|| Ok(())).unwrap();
        let changed = base
            .adopt(
                base.capture_edit()
                    .unwrap()
                    .visibility(&layer, false, &|| Ok(()))
                    .unwrap(),
            )
            .unwrap();
        assert!(changed.publish("worker", &[], 1, false, prepared).is_err());
    }
    #[test]
    fn all_intermediate_pixel_outputs_remain_unpublished_and_retire_on_late_refusal() {
        use super::super::output::tests::{draft, Session};
        let pool = ResidentBudget::new(16 * 1024 * 1024).unwrap();
        let base = super::super::tests::document(&pool);
        let layer = base.metadata().layer(0).unwrap().id().to_owned();
        let commands = [
            json!({"op":"pixels.replace","layer":layer,"rect":[0,0,8,8]}),
            json!({"op":"pixels.replace","layer":layer,"rect":[0,0,8,8]}),
        ];
        let mut transaction = begin(&base, &commands);
        let step = transaction.next(&|| Ok(())).unwrap();
        let (draft, path) = draft(step.document());
        let output = draft
            .seal(step.document(), &Session, &|| Ok(()))
            .unwrap_or_else(|e| panic!("{}", e.reason()));
        let slot = step.document().layer_slot(&layer).unwrap();
        let delta = step
            .raster_patch(
                RasterPatch {
                    target: slot,
                    output,
                    retained: RetainedUpdate::Clear,
                    layer_origin: None,
                },
                &|| Ok(()),
            )
            .unwrap_or_else(|e| panic!("{}", e.reason()));
        transaction.complete(delta, &|| Ok(())).unwrap();
        assert!(base.disk_budget().used().unwrap() > 0);
        let step = transaction.next(&|| Ok(())).unwrap();
        let output = OutputDraft::new(Surface::zero(8, 8, 16).unwrap(), vec![])
            .seal(step.document(), &Session, &|| Ok(()))
            .unwrap_or_else(|e| panic!("{}", e.reason()));
        let slot = step.document().layer_slot(&layer).unwrap();
        let delta = step
            .raster_patch(
                RasterPatch {
                    target: slot,
                    output,
                    retained: RetainedUpdate::Clear,
                    layer_origin: None,
                },
                &|| Ok(()),
            )
            .unwrap_or_else(|e| panic!("{}", e.reason()));
        transaction.complete(delta, &|| Ok(())).unwrap();
        let prepared = transaction.finish(&|| Ok(())).unwrap();
        prepared.cancellation.cancel();
        let refused = base.publish("worker", &[], 1, false, prepared).unwrap_err();
        refused.abort(&Session, &|| Ok(())).unwrap();
        assert_eq!(base.disk_budget().used().unwrap(), 0);
        assert!(base
            .source(base.layer_slot(&layer).unwrap())
            .unwrap()
            .is_known_zero());
        std::fs::remove_dir(path).unwrap();
        drop(base);
        assert_eq!(pool.used(), 0);
    }
}
fn restamp(
    source: &NativeDocument,
    revision: u64,
    check: &dyn Fn() -> Result<()>,
) -> Result<NativeDocument> {
    let metadata = copy_metadata(&source.core.metadata, source.budget(), check)?;
    let roster = source.core.roster.clone();
    let resident = source.budget().clone();
    let disk = source.disk_budget().clone();
    let core = metadata.map_unique(|mut metadata, _| {
        check()?;
        metadata.revision = revision;
        Ok(Core {
            metadata,
            roster,
            resident,
            disk,
            key: key(),
            render_key: OnceLock::new(),
        })
    })?;
    Ok(NativeDocument { core })
}
