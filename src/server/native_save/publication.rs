//! Both filesystem writers share the same Engine/save/history finalization.
use crate::{
    file_safety::{
        ExternalOutcomeProof, NativeAbortReceipt, NativeExistingAbortReceipt,
        NativeExistingDestination, NativeExistingPublicationReceipt, NativeInventory,
        NativeNewDestination, NativePublicationReceipt, Version,
    },
    psd::save36::CompletedSave36,
    workspace::source_runtime::{CommittedPublication, PublicationIntent, SourceSession},
};
type Check<'a> = &'a dyn Fn() -> Result<(), String>;
pub(super) enum Writer<'inventory, 'lease, 'session> {
    New(NativeNewDestination<'inventory, 'lease, 'session>),
    Existing(NativeExistingDestination<'inventory, 'lease, 'session>),
}
impl<'inventory, 'lease, 'session> Writer<'inventory, 'lease, 'session> {
    pub(super) fn create(
        session: &'session SourceSession,
        candidate: &CompletedSave36<'_>,
        inventory: &'inventory mut NativeInventory<'lease>,
        intent: &PublicationIntent<'_>,
        existing: bool,
        check: Check<'_>,
    ) -> Result<Self, String> {
        if existing {
            NativeExistingDestination::create(session, candidate, inventory, intent, check)
                .map(Self::Existing)
        } else {
            NativeNewDestination::create(session, candidate, inventory, intent, check)
                .map(Self::New)
        }
    }
    pub(super) fn replay(
        &mut self,
        candidate: &CompletedSave36<'_>,
        check: Check<'_>,
        progress: &mut dyn FnMut(u64) -> Result<(), String>,
    ) -> Result<(), String> {
        match self {
            Self::New(w) => w.replay(candidate, check, progress),
            Self::Existing(w) => w.replay(candidate, check, progress),
        }
    }
    pub(super) fn ready_to_arm(&self, check: Check<'_>) -> Result<(), String> {
        match self {
            Self::New(w) => w.ready_to_arm(check),
            Self::Existing(w) => w.ready_to_arm(check),
        }
    }
    pub(super) fn abort(
        self,
        check: Check<'_>,
    ) -> Result<Abort<'inventory, 'lease, 'session>, String> {
        match self {
            Self::New(w) => w.abort(check).map(Abort::New),
            Self::Existing(w) => w.abort(check).map(Abort::Existing),
        }
    }
    pub(super) fn publish(
        self,
        committed: &CommittedPublication<'_>,
        check: Check<'_>,
    ) -> Result<Receipt<'inventory, 'lease, 'session>, String> {
        match self {
            Self::New(w) => w.publish(committed, check).map(Receipt::New),
            Self::Existing(w) => w.publish(committed, check).map(Receipt::Existing),
        }
    }
}
pub(super) enum Abort<'inventory, 'lease, 'session> {
    New(NativeAbortReceipt<'inventory, 'lease>),
    Existing(NativeExistingAbortReceipt<'inventory, 'lease, 'session>),
}
impl Abort<'_, '_, '_> {
    pub(super) fn proof(&self) -> &dyn ExternalOutcomeProof {
        match self {
            Self::New(receipt) => receipt,
            Self::Existing(receipt) => receipt,
        }
    }
}
pub(super) enum Receipt<'inventory, 'lease, 'session> {
    New(NativePublicationReceipt<'inventory, 'lease>),
    Existing(NativeExistingPublicationReceipt<'inventory, 'lease, 'session>),
}
impl Receipt<'_, '_, '_> {
    pub(super) fn proof(&self) -> &dyn ExternalOutcomeProof {
        match self {
            Self::New(receipt) => receipt,
            Self::Existing(receipt) => receipt,
        }
    }
    pub(super) fn version(&self) -> Version {
        match self {
            Self::New(receipt) => receipt.version(),
            Self::Existing(receipt) => receipt.version(),
        }
    }
}
