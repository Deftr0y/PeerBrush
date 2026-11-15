//! Save metadata changes neither revision nor history entries. The prepared
//! owner grants no filesystem publication permission and retires off-lock.
use super::*;
#[cfg(test)]
#[path = "save_name_tests.rs"]
mod tests;

pub(crate) struct PreparedSaveName {
    history: DocumentKey,
    epoch: u64,
    base: DocumentKey,
    candidate: NativeDocument,
}
#[must_use = "Retire the refused name owner after releasing Engine"]
pub(crate) struct RefusedSaveName {
    reason: String,
    prepared: PreparedSaveName,
}
impl std::fmt::Debug for RefusedSaveName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("RefusedSaveName")
            .field(&self.reason)
            .finish()
    }
}
impl RefusedSaveName {
    pub(crate) fn reason(&self) -> &str {
        &self.reason
    }
    pub(crate) fn into_prepared(self) -> PreparedSaveName {
        self.prepared
    }
}
impl PreparedSaveName {
    pub(crate) fn refused(self, reason: String) -> RefusedSaveName {
        RefusedSaveName {
            reason,
            prepared: self,
        }
    }
}
impl HistorySnapshot {
    pub(crate) fn prepare_save_name(
        &self,
        current: &NativeDocument,
        name: &str,
        check: &dyn Fn() -> Result<()>,
    ) -> Result<PreparedSaveName> {
        check()?;
        if !self.pool.same_pool(current.budget())
            || self.current.is_some_and(|key| key != current.key())
            || current.metadata().read_only()
            || self.epoch == u64::MAX
            || name.is_empty()
            || name.len() > 4096
            || name.contains('\0')
        {
            return Err("Native saved name differs from its editable history or admission".into());
        }
        let candidate = super::super::wand::restamp_named(
            current,
            current.metadata().revision(),
            name,
            current.core.wand.clone(),
            check,
        )?;
        check()?;
        Ok(PreparedSaveName {
            history: self.identity,
            epoch: self.epoch,
            base: current.key(),
            candidate,
        })
    }
}
impl NativeHistory {
    pub(crate) fn validate_save_name(
        &self,
        current: &NativeDocument,
        prepared: &PreparedSaveName,
    ) -> Result<()> {
        if self.identity != prepared.history
            || self.epoch != prepared.epoch
            || self.epoch == u64::MAX
            || self.current.is_some_and(|key| key != current.key())
            || prepared.base != current.key()
            || !self.pool.same_pool(current.budget())
            || current.metadata().read_only()
            || !prepared.candidate.budget().same_pool(current.budget())
            || !prepared
                .candidate
                .disk_budget()
                .same_pool(current.disk_budget())
            || !Arc::ptr_eq(&prepared.candidate.core.roster, &current.core.roster)
            || prepared.candidate.metadata().id() != current.metadata().id()
            || prepared.candidate.metadata().revision() != current.metadata().revision()
        {
            return Err("Native saved name source or history cursor changed".into());
        }
        Ok(())
    }
    pub(crate) fn commit_save_name(
        &mut self,
        current: &mut NativeDocument,
        prepared: PreparedSaveName,
    ) -> std::result::Result<NativeDocument, RefusedSaveName> {
        if let Err(reason) = self.validate_save_name(current, &prepared) {
            return Err(prepared.refused(reason));
        }
        self.epoch += 1;
        self.current = Some(prepared.candidate.key());
        self.name_from_save = true;
        Ok(std::mem::replace(current, prepared.candidate))
    }
}
