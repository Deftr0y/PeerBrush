//! Source reachability is planned by the worker; final marks are memory-only.
//! Unreferenced provisional bytes retire explicitly after the Engine lock.
use super::output::Retirement;
use super::*;
use crate::{engine::scene::StageSession, large_document::Reservation};

pub(super) struct SourcePublication {
    candidate: DocumentKey,
    surviving: Vec<bool>,
    retired: Vec<Retirement>,
    _charge: Reservation,
}

#[cfg(test)]
mod tests {
    use super::super::output::tests::{draft, Session};
    use super::*;
    use serde_json::json;
    fn prepared(
        base: &NativeDocument,
        erase: bool,
        escape: bool,
    ) -> (PreparedDelta, std::path::PathBuf, Option<NativeDocument>) {
        let layer = base.metadata().layer(0).unwrap().id().to_owned();
        let command = json!({"op":"pixels.replace","layer":layer,"rect":[0,0,8,8]});
        let commands = if erase {
            vec![command.clone(), command]
        } else {
            vec![command]
        };
        let descriptor =
            CommandDescriptor::from_normalized(base, 0, &commands, &|| Ok(())).unwrap();
        let mut transaction = base
            .capture_descriptor(descriptor, "worker", &[], 1, false, true)
            .unwrap()
            .transaction()
            .unwrap();
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
        let escaped = escape.then(|| delta.preview().clone());
        transaction.complete(delta, &|| Ok(())).unwrap();
        if erase {
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
        }
        (transaction.finish(&|| Ok(())).unwrap(), path, escaped)
    }
    fn commit(
        base: &mut NativeDocument,
        delta: PreparedDelta,
        history: &mut NativeHistory,
    ) -> RetiredHistory {
        let commit = history
            .snapshot()
            .prepare_commit(
                base,
                delta,
                HistoryRequest {
                    actor: "worker",
                    label: "Pixels",
                    task: Some("task"),
                    gesture: None,
                },
                &|| Ok(()),
            )
            .unwrap();
        history
            .commit(
                base,
                Publication {
                    actor: "worker",
                    leases: &[],
                    now: 1,
                    closed: false,
                    write_available: true,
                },
                commit,
            )
            .unwrap()
    }
    #[test]
    fn successful_batch_retires_only_unreachable_intermediate_fragments_after_publication() {
        let pool = ResidentBudget::new(16 * 1024 * 1024).unwrap();
        let mut base = super::super::tests::document(&pool);
        let (delta, path, _) = prepared(&base, true, false);
        let mut history = NativeHistory::new(&pool, 20).unwrap();
        let retired = commit(&mut base, delta, &mut history);
        assert!(retired.has_provisional_sources());
        assert!(base.disk_budget().used().unwrap() > 0);
        assert_eq!(base.metadata().revision(), 1);
        assert_eq!(history.undo_len(), 1);
        retired.retire(&Session, &|| Ok(())).unwrap();
        assert_eq!(base.disk_budget().used().unwrap(), 0);
        assert!(!path.join("blocks.pbt").exists());
        std::fs::remove_dir(path).unwrap();
        drop(base);
        drop(history);
        assert_eq!(pool.used(), 0);
    }
    #[test]
    fn surviving_fragment_marks_in_place_and_remains_owned_by_document_and_history() {
        let pool = ResidentBudget::new(16 * 1024 * 1024).unwrap();
        let mut base = super::super::tests::document(&pool);
        let (delta, path, _) = prepared(&base, false, false);
        let mut history = NativeHistory::new(&pool, 20).unwrap();
        let retired = commit(&mut base, delta, &mut history);
        let bytes = base.disk_budget().used().unwrap();
        retired.retire(&Session, &|| Ok(())).unwrap();
        let source = base
            .source(base.metadata().layer(0).unwrap().pixels().slot())
            .unwrap();
        assert!(source
            .physical_tiles()
            .next()
            .unwrap()
            .owner()
            .publication_marked());
        assert!(path.join("blocks.pbt").exists());
        assert_eq!(base.disk_budget().used().unwrap(), bytes);
        assert_eq!(
            source
                .prepare_tile((0, 0), &mut || Ok(()))
                .unwrap()
                .bytes()
                .unwrap()[..2],
            12345u16.to_be_bytes()
        );
        drop(base);
        drop(history);
        assert_eq!(pool.used(), 0);
        std::fs::remove_file(path.join("blocks.pbt")).unwrap();
        std::fs::remove_dir(path).unwrap();
    }
    #[test]
    fn escaped_intermediate_preview_refuses_retirement_and_keeps_debt_and_bytes() {
        let pool = ResidentBudget::new(16 * 1024 * 1024).unwrap();
        let mut base = super::super::tests::document(&pool);
        let (delta, path, escaped) = prepared(&base, true, true);
        let escaped = escaped.unwrap();
        let mut history = NativeHistory::new(&pool, 20).unwrap();
        let retired = commit(&mut base, delta, &mut history);
        let bytes = base.disk_budget().used().unwrap();
        assert!(retired.retire(&Session, &|| Ok(())).is_err());
        assert_eq!(base.disk_budget().used().unwrap(), bytes);
        assert!(path.join("blocks.pbt").exists());
        let source = escaped
            .source(escaped.metadata().layer(0).unwrap().pixels().slot())
            .unwrap();
        assert!(!source
            .physical_tiles()
            .next()
            .unwrap()
            .owner()
            .publication_marked());
        assert_eq!(
            source
                .prepare_tile((0, 0), &mut || Ok(()))
                .unwrap()
                .bytes()
                .unwrap()[..2],
            12345u16.to_be_bytes()
        );
        assert!(base
            .source(base.metadata().layer(0).unwrap().pixels().slot())
            .unwrap()
            .is_known_zero());
        drop(escaped);
        assert_eq!(base.disk_budget().used().unwrap(), bytes);
        drop(base);
        drop(history);
        assert_eq!(pool.used(), 0);
        std::fs::remove_file(path.join("blocks.pbt")).unwrap();
        std::fs::remove_dir(path).unwrap();
    }
}
impl SourcePublication {
    pub(super) fn prepare(
        candidate: &NativeDocument,
        outputs: &Vec<StagedOutput>,
        check: &dyn Fn() -> Result<()>,
    ) -> Result<Self> {
        check()?;
        let physical = candidate
            .owned_sources()
            .try_fold(0usize, |count, source| {
                count
                    .checked_add(source.physical_tiles().count())
                    .ok_or("Native publication source count overflow")
            })?;
        let fragments = outputs.iter().try_fold(0usize, |count, output| {
            count
                .checked_add(output.fragments().len())
                .ok_or("Native publication fragment count overflow")
        })?;
        let bytes = physical
            .checked_mul(16)
            .and_then(|n| n.checked_add(fragments))
            .and_then(|n| {
                outputs
                    .capacity()
                    .checked_mul(std::mem::size_of::<StagedOutput>())
                    .and_then(|o| n.checked_add(o))
            })
            .and_then(|n| {
                outputs
                    .len()
                    .checked_mul(std::mem::size_of::<Retirement>())
                    .and_then(|r| n.checked_add(r))
            })
            .and_then(|n| n.checked_add(512))
            .ok_or("Native publication footprint overflow")?;
        let charge = candidate
            .budget()
            .reserve_bytes(bytes, bytes)
            .map_err(|e| e.to_string())?;
        let mut owners = Vec::new();
        owners
            .try_reserve_exact(physical)
            .map_err(|_| "Native publication owner allocation refused")?;
        if owners.capacity() > physical {
            return Err("Native publication owner allocation exceeds admission".into());
        }
        for surface in candidate.owned_sources() {
            for tile in surface.physical_tiles() {
                check()?;
                owners.push(tile.owner().identity());
            }
        }
        owners.sort_unstable();
        owners.dedup();
        let mut surviving = Vec::new();
        surviving
            .try_reserve_exact(fragments)
            .map_err(|_| "Native publication flag allocation refused")?;
        if surviving.capacity() > fragments {
            return Err("Native publication flags exceed admission".into());
        }
        for output in outputs {
            for fragment in output.fragments() {
                check()?;
                surviving.push(owners.binary_search(&fragment.store().identity()).is_ok());
            }
        }
        let mut retired = Vec::new();
        retired
            .try_reserve_exact(outputs.len())
            .map_err(|_| "Native publication retirement allocation refused")?;
        if retired.capacity() > outputs.len() {
            return Err("Native publication retirement exceeds admission".into());
        }
        check()?;
        Ok(Self {
            candidate: candidate.key(),
            surviving,
            retired,
            _charge: charge,
        })
    }
    pub(super) fn validate(
        &self,
        candidate: &NativeDocument,
        outputs: &[StagedOutput],
    ) -> Result<()> {
        if self.candidate != candidate.key()
            || self.retired.capacity() < outputs.len()
            || self.surviving.len()
                != outputs
                    .iter()
                    .map(|output| output.fragments().len())
                    .sum::<usize>()
        {
            return Err("Native publication source plan is stale".into());
        }
        Ok(())
    }
    /// Called after final document/history guards. Stores and all output views
    /// remain held until post-lock cleanup; this cannot close a source file.
    pub(super) fn mark(&self, outputs: &[StagedOutput]) {
        for (fragment, survives) in outputs
            .iter()
            .flat_map(StagedOutput::fragments)
            .zip(&self.surviving)
        {
            if *survives {
                fragment.mark_published();
            }
        }
    }
    pub(super) fn retire(
        mut self,
        outputs: Vec<StagedOutput>,
        session: &impl StageSession,
        cleanup: &dyn Fn() -> Result<()>,
    ) -> Result<()> {
        for output in outputs {
            self.retired.push(output.into_retirement());
        }
        // Every Surface and Snapshot view has now dropped, including views in
        // other outputs sharing one fragment. Escaped previews refuse retirement.
        let mut flags = self.surviving.iter();
        let mut error = None;
        for retirement in self.retired {
            if let Err(reason) = retirement.retire_selected(&mut flags, session, cleanup) {
                error.get_or_insert(reason);
            }
        }
        error.map_or(Ok(()), Err)
    }
}
