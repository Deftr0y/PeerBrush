//! Immutable native backing blocks. Admission and pixel access stay explicit.
//!
//! This module does not raise canvas/file limits or turn the existing infallible
//! raster getters into implicit disk reads. Callers must pin a bounded region
//! successfully before sampling it, and retain dirty source bytes until put succeeds.
mod access;
pub(crate) use access::{
    Access as SourceAccess, Exclusive as SourceExclusive, OwnedRead as SourceRead,
};
mod budget;
mod bytes;
mod disk;
mod index;
mod linear;
pub(crate) use linear::unlink_owned_at;
mod planar;
pub(crate) mod root;
mod scalar;
pub(crate) mod segments;
mod store;

pub(crate) use budget::DerivedCache;
pub use budget::{Reservation, ResidentBudget};
pub(crate) use bytes::{verified_transfer_buffer, BufferedWrite, VERIFIED_BATCH_BYTES};
pub use bytes::{
    AppendWriter, ByteRole, BytesCursor, CompletedBytes, LargeSourceTemporary, MAX_SOURCE_PAYLOAD,
};
pub(crate) use disk::{
    CoexistenceSettlement, CoexistenceTicket, ExternalFact, PreparedSettlement, PublicationTicket,
};
pub use disk::{DiskBudget, DiskReservation};
pub use planar::{CompletedPlanar, PlanarRole, PlanarScratch, PlanarSpec};
pub use scalar::{CompletedScalar, ScalarScratch, ScalarSpec};
pub use store::{
    BlockRef, CommitRef, FreshFragment, Limits, PinnedBytes, PinnedTile, ScratchBytes, Snapshot,
    Store, TileSpec,
};
pub(crate) use store::{
    ParkedSource, RefusedRetirement as RefusedFragmentRetirement, RetiredSource, WeakStore,
};

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug)]
pub enum Error {
    Invalid(&'static str),
    Budget,
    Busy,
    Changed,
    Corrupt,
    Cancelled,
    Stale,
    Interrupted,
    Io(std::io::Error),
}

impl From<std::io::Error> for Error {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

impl std::fmt::Display for Error {
    fn fmt(&self, output: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        output.write_str(match self {
            Self::Invalid(message) => message,
            Self::Budget => "Native backing byte or record budget exceeded",
            Self::Busy => "Native backing is busy; retry without changing the document",
            Self::Changed => "Native backing ownership or bytes changed; current work retained",
            Self::Corrupt => "Native backing data failed its exact integrity checks",
            Self::Cancelled => "Native backing operation cancelled before publication",
            Self::Stale => "Native backing checkpoint no longer matches its source parent",
            Self::Interrupted => "Native backing has an unconfirmed write; recovery remains held",
            Self::Io(_) => "Native backing I/O failed; current work retained",
        })
    }
}

impl std::error::Error for Error {}
