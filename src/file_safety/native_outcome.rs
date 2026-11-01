//! Closed filesystem outcome boundary for external-disk accounting.
//! Raw fact records and journal checksums never implement this authority.
use super::NativeExternalFile;
use std::{ffi::{OsStr, OsString}, path::{Path, PathBuf}};

/// Internal name binding; only the closed writer can mint a production value
/// from the durable publication plan. A raw name is never backup authority.
pub(super) struct BackupPlan {
    target: PathBuf,
    expected: Option<super::Version>,
    name: Option<OsString>,
}
impl BackupPlan {
    pub(super) fn target(&self) -> &Path { &self.target }
    pub(super) fn expected(&self) -> Option<super::Version> { self.expected }
    pub(super) fn name(&self) -> Option<&OsStr> { self.name.as_deref() }
    #[cfg(test)]
    pub(super) fn for_tests(target: PathBuf, expected: Option<super::Version>, name: Option<OsString>) -> Self {
        Self { target, expected, name }
    }
}

mod sealed {
    pub trait RootOutcome {}
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ExternalCreationRole {
    Temporary,
    Backup,
}
/// A live zero-byte exclusive owner. A planned name or copied metadata fact
/// cannot implement this sealed authority, even when its digest happens to fit.
pub(crate) trait ExternalCreationProof: sealed::RootOutcome {
    fn validate(&self, intent: [u8; 32], target: [u8; 32], growth: u64) -> Result<(), String>;
    fn fact(&self) -> NativeExternalFile;
    fn role(&self) -> ExternalCreationRole;
    fn name(&self) -> &std::ffi::OsStr;
    fn parent_identity(&self) -> (u64, u64);
}
/// Implemented by the native destination writer's held publication/abort
/// receipts, after all named identities and retired owners are reobserved.
/// Validation may perform I/O and therefore precedes the ledger mutex.
pub(crate) trait ExternalOutcomeProof: sealed::RootOutcome {
    fn validate(&self, intent: [u8; 32], target: [u8; 32], growth: u64) -> Result<(), String>;
    fn facts(&self) -> &[NativeExternalFile];
    fn retired(&self) -> &[(u64, u64)];
}

// Concrete filesystem receipts belong to this module's native writer. Until
// that writer is integrated there is intentionally no production implementor.

#[cfg(test)]
pub(crate) struct AccountingOutcome {
    intent: [u8; 32],
    target: [u8; 32],
    growth: u64,
    facts: Vec<NativeExternalFile>,
    retired: Vec<(u64, u64)>,
    accepted: bool,
}
#[cfg(test)]
impl AccountingOutcome {
    /// Accounting-only fixture. This cannot qualify filesystem publication,
    /// source integrity or recovery authority; it is absent from production.
    pub(crate) fn new(
        intent: [u8; 32],
        target: [u8; 32],
        growth: u64,
        facts: Vec<NativeExternalFile>,
        retired: Vec<(u64, u64)>,
    ) -> Self {
        Self {
            intent,
            target,
            growth,
            facts,
            retired,
            accepted: true,
        }
    }
    pub(crate) fn refuse(mut self) -> Self {
        self.accepted = false;
        self
    }
}
#[cfg(test)]
impl sealed::RootOutcome for AccountingOutcome {}
#[cfg(test)]
impl ExternalOutcomeProof for AccountingOutcome {
    fn validate(&self, intent: [u8; 32], target: [u8; 32], growth: u64) -> Result<(), String> {
        if self.accepted && (self.intent, self.target, self.growth) == (intent, target, growth) {
            Ok(())
        } else {
            Err("Accounting fixture outcome refused".into())
        }
    }
    fn facts(&self) -> &[NativeExternalFile] {
        &self.facts
    }
    fn retired(&self) -> &[(u64, u64)] {
        &self.retired
    }
}

#[cfg(test)]
pub(crate) struct AccountingCreation {
    intent: [u8;32], target: [u8;32], growth:u64,
    fact:NativeExternalFile, role:ExternalCreationRole, name:std::ffi::OsString,
    parent:(u64,u64), accepted:bool,
}
#[cfg(test)]
impl AccountingCreation {
    /// Journal admission fixture only; absent from production, no file authority.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(intent:[u8;32],target:[u8;32],growth:u64,fact:NativeExternalFile,
        role:ExternalCreationRole,name:std::ffi::OsString,parent:(u64,u64))->Self {
        Self {intent,target,growth,fact,role,name,parent,accepted:true}
    }
    pub(crate) fn refuse(mut self)->Self {self.accepted=false;self}
}
#[cfg(test)]
impl sealed::RootOutcome for AccountingCreation {}
#[cfg(test)]
impl ExternalCreationProof for AccountingCreation {
    fn validate(&self,i:[u8;32],t:[u8;32],g:u64)->Result<(),String> {
        if self.accepted && self.fact.bytes()==0 && (i,t,g)==(self.intent,self.target,self.growth) {Ok(())}
        else {Err("Accounting creation fixture refused".into())}
    }
    fn fact(&self)->NativeExternalFile {self.fact}
    fn role(&self)->ExternalCreationRole {self.role}
    fn name(&self)->&std::ffi::OsStr {&self.name}
    fn parent_identity(&self)->(u64,u64) {self.parent}
}
