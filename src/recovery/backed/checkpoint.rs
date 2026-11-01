//! Constructible only after actual recovery framing/source proof.
use super::*;

pub(crate) struct Proof {
    snapshot: [u8; 16],
    bytes: u64,
    sha256: [u8; 32],
    sources: Vec<Snapshot>,
    _owner: Arc<MetadataCharge>,
}
pub(crate) struct Parts {
    pub snapshot: [u8; 16],
    pub bytes: u64,
    pub sha256: [u8; 32],
    pub sources: Vec<Snapshot>,
    pub owner: Arc<MetadataCharge>,
}
impl Proof {
    pub(super) fn from_validated(
        snapshot: [u8; 16],
        bytes: u64,
        sha256: [u8; 32],
        sources: impl ExactSizeIterator<Item = Snapshot>,
        session: &SourceSession,
        check: &dyn Fn() -> Result<()>,
    ) -> Result<Self> {
        if snapshot == [0; 16] || bytes == 0 || bytes > MAX_MARKER || sources.len() > MAX_STORES {
            return Err("Checkpoint proof admission differs".into());
        }
        let admitted = sources
            .len()
            .checked_mul(2 * std::mem::size_of::<Snapshot>())
            .and_then(|n| n.checked_add(128))
            .ok_or("Checkpoint source roster overflow")?;
        let owner = MetadataCharge::reserve(&session.resident(), admitted, check)?;
        let count = sources.len();
        let mut selected = Vec::new();
        selected
            .try_reserve_exact(count)
            .map_err(|_| "Checkpoint source allocation refused")?;
        if selected
            .capacity()
            .checked_mul(std::mem::size_of::<Snapshot>())
            .is_none_or(|n| n > owner.bytes())
        {
            return Err("Checkpoint source capacity exceeds admission".into());
        }
        let mut previous = None;
        for source in sources {
            check()?;
            session.check()?;
            let identity = source.store().identity();
            if previous.is_some_and(|id| id >= identity)
                || !source.store().publication_marked()
                || !source
                    .store()
                    .resident_budget()
                    .same_pool(&session.resident())
                || !source.store().uses_disk_budget(&session.disk())
            {
                return Err(
                    "Checkpoint sources are unordered, unpublished or outside admission".into(),
                );
            }
            session.check_owned_checkpoint_source(&source, check)?;
            previous = Some(identity);
            if selected.len() == count {
                return Err("Checkpoint source roster exceeded its admission".into());
            }
            selected.push(source);
        }
        if selected.len() != count {
            return Err("Checkpoint source roster is incomplete".into());
        }
        Ok(Self {
            snapshot,
            bytes,
            sha256,
            sources: selected,
            _owner: owner,
        })
    }
    pub(crate) fn into_parts(self) -> Parts {
        let Self {
            snapshot,
            bytes,
            sha256,
            sources,
            _owner,
        } = self;
        Parts {
            snapshot,
            bytes,
            sha256,
            sources,
            owner: _owner,
        }
    }
}
