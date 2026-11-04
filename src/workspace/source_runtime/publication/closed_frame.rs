//! Fixed-size closed quota record. This data cannot implement a Root outcome.
use super::frame::{IntentFrame, Role, MAX_FACTS};
use crate::{
    large_document::{ExternalFact, ResidentBudget, ScratchBytes},
    psd::memory36::MetadataCharge,
};
use ring::digest::{Context, SHA256};
use std::{
    io::{Cursor, Write},
    sync::Arc,
};

pub(super) const BYTES: usize = 256 * 1024;
pub(super) const MAX_BINDINGS: usize = MAX_FACTS + 2;
const MAGIC: &[u8; 16] = b"PBEXCLOSED01\0\0\0\0";
const DOMAIN: &[u8] = b"PeerBrushExternalClosedFrame1\0";
type Result<T> = std::result::Result<T, String>;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Place {
    Target,
    BackupOrdinal(u32),
    CreatedBackup,
    Temporary,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct Binding {
    pub place: Place,
    pub fact: ExternalFact,
}
pub(super) struct ClosedFrame {
    pub id: [u8; 16],
    pub owner: [u8; 16],
    pub intent: [u8; 32],
    pub target: [u8; 32],
    pub growth: u64,
    pub bindings: Vec<Binding>,
    pub retired: Vec<(u64, u64)>,
}
/// This private pair never exposes an independently clonable payload Arc.
/// Mutable access is possible only before its first shared registration.
pub(super) struct ChargedClosed {
    value: Arc<ClosedFrame>,
    _owner: Arc<MetadataCharge>,
}
impl Clone for ChargedClosed {
    fn clone(&self) -> Self {
        Self {
            value: self.value.clone(),
            _owner: self._owner.clone(),
        }
    }
}
impl std::ops::Deref for ChargedClosed {
    type Target = ClosedFrame;
    fn deref(&self) -> &ClosedFrame {
        &self.value
    }
}
impl ChargedClosed {
    pub(super) fn prepared(value: ClosedFrame, owner: Arc<MetadataCharge>) -> Self {
        Self {
            value: Arc::new(value),
            _owner: owner,
        }
    }
    pub(super) fn get_mut(&mut self) -> Result<&mut ClosedFrame> {
        Arc::get_mut(&mut self.value).ok_or_else(|| "Closed record is already shared".into())
    }
    pub(super) fn uses_budget(&self, pool: &ResidentBudget) -> bool {
        self._owner.uses_budget(pool)
    }
}
impl ClosedFrame {
    pub(super) fn incomplete(bytes: &[u8], original: &IntentFrame, intent: [u8; 32]) -> Result<()> {
        if bytes.len() >= BYTES {
            return Err("Closed record is not an incomplete slot".into());
        }
        let mut known = [0u8; 120];
        let mut out = Cursor::new(&mut known[..]);
        put(&mut out, MAGIC)?;
        put(&mut out, &original.id)?;
        put(&mut out, &original.owner)?;
        put(&mut out, &intent)?;
        put(&mut out, &original.target_key)?;
        put(&mut out, &original.growth.to_be_bytes())?;
        let count = bytes.len().min(known.len());
        if bytes[..count] != known[..count] {
            return Err("Incomplete outcome has an unknown intent/domain; retained".into());
        }
        if bytes.len() >= 124
            && u32::from_be_bytes(bytes[120..124].try_into().unwrap()) as usize > MAX_BINDINGS
        {
            return Err("Incomplete outcome index exceeds admission".into());
        }
        if bytes.len() >= 125 && bytes[124] > 3 {
            return Err("Incomplete outcome retirement count exceeds admission".into());
        }
        Ok(())
    }
    pub(super) fn validate(&self, original: &IntentFrame, intent: [u8; 32]) -> Result<()> {
        if self.id != original.id
            || self.owner != original.owner
            || self.intent != intent
            || intent == [0; 32]
            || self.target != original.target_key
            || self.growth != original.growth
            || self.bindings.len() > MAX_BINDINGS
            || self.retired.len() > 3
            || self
                .bindings
                .windows(2)
                .any(|w| w[0].fact.identity >= w[1].fact.identity)
            || self
                .retired
                .iter()
                .enumerate()
                .any(|(at, id)| self.retired[..at].contains(id))
        {
            return Err("Closed external record differs from its exact intent".into());
        }
        let mut target = false;
        let mut created = false;
        let mut temporary = false;
        let mut backups = 0;
        for (at, binding) in self.bindings.iter().enumerate() {
            if self.retired.contains(&binding.fact.identity)
                || self.bindings[..at].iter().any(|b| b.place == binding.place)
            {
                return Err("Closed external binding is duplicated or retired".into());
            }
            match binding.place {
                Place::BackupOrdinal(ordinal) => {
                    let old = original
                        .facts
                        .get(ordinal as usize)
                        .ok_or("Closed backup ordinal is absent")?;
                    if old.role != Role::Backup || old.fact != binding.fact {
                        return Err("Previously retained backup changed".into());
                    }
                    backups += 1;
                }
                Place::Target => {
                    target = true;
                    match original.expected {
                        Some(old) if binding.fact.identity == old.identity => {
                            if binding.fact.bytes != old.bytes
                                || binding.fact.modified != old.modified
                            {
                                return Err("Old destination was modified in place".into());
                            }
                        }
                        _ if binding.fact.bytes == original.candidate_bytes => (),
                        _ => {
                            return Err(
                                "Closed destination length differs from its candidate".into()
                            )
                        }
                    }
                }
                Place::CreatedBackup => {
                    created = true;
                    if original.backup.is_none()
                        || original.expected.is_none_or(|old| {
                            old.bytes != binding.fact.bytes || old.identity == binding.fact.identity
                        })
                    {
                        return Err(
                            "New backup does not bind a separate complete source copy".into()
                        );
                    }
                }
                Place::Temporary => {
                    temporary = true;
                    if binding.fact.bytes != original.candidate_bytes {
                        return Err("Partial temporary cannot settle external debt".into());
                    }
                }
            }
        }
        if backups
            != original
                .facts
                .iter()
                .filter(|f| f.role == Role::Backup)
                .count()
            || (!target && original.expected.is_some())
            || usize::from(target) + usize::from(created) + usize::from(temporary) + backups
                != self.bindings.len()
            || self.retired.iter().any(|id| {
                original
                    .facts
                    .iter()
                    .any(|old| old.fact.identity == *id && old.role == Role::Backup)
            })
        {
            return Err("Closed external inventory omits retained files".into());
        }
        Ok(())
    }
    /// Fixed final length was admitted before commit. Only forward writes are
    /// made into the caller's same-pool charged buffer; zero padding is hashed.
    pub(super) fn encode(
        &self,
        original: &IntentFrame,
        intent: [u8; 32],
        out: &mut ScratchBytes,
        pool: &ResidentBudget,
        check: &dyn Fn() -> Result<()>,
    ) -> Result<[u8; 32]> {
        self.validate(original, intent)?;
        if out.len() != BYTES || !out.uses_budget(pool) {
            return Err("Closed external buffer differs from its preadmission".into());
        }
        out.fill(0);
        let mut cursor = Cursor::new(&mut out[..BYTES - 32]);
        put(&mut cursor, MAGIC)?;
        put(&mut cursor, &self.id)?;
        put(&mut cursor, &self.owner)?;
        put(&mut cursor, &self.intent)?;
        put(&mut cursor, &self.target)?;
        put(&mut cursor, &self.growth.to_be_bytes())?;
        put(&mut cursor, &(self.bindings.len() as u32).to_be_bytes())?;
        put(&mut cursor, &[self.retired.len() as u8])?;
        for binding in &self.bindings {
            check()?;
            let (tag, ordinal) = match binding.place {
                Place::Target => (1, 0),
                Place::BackupOrdinal(n) => (2, n),
                Place::CreatedBackup => (3, 0),
                Place::Temporary => (4, 0),
            };
            put(&mut cursor, &[tag])?;
            put(&mut cursor, &ordinal.to_be_bytes())?;
            id(&mut cursor, binding.fact.identity)?;
            put(&mut cursor, &binding.fact.bytes.to_be_bytes())?;
            put(&mut cursor, &binding.fact.modified.to_be_bytes())?;
        }
        for identity in &self.retired {
            id(&mut cursor, *identity)?;
        }
        drop(cursor);
        let mut hash = Context::new(&SHA256);
        hash.update(DOMAIN);
        for chunk in out[..BYTES - 32].chunks(65536) {
            check()?;
            hash.update(chunk);
        }
        out[BYTES - 32..].copy_from_slice(hash.finish().as_ref());
        check()?;
        Ok(ring::digest::digest(&SHA256, out)
            .as_ref()
            .try_into()
            .unwrap())
    }
    pub(super) fn decode(
        bytes: &[u8],
        original: &IntentFrame,
        intent: [u8; 32],
        pool: &ResidentBudget,
        check: &dyn Fn() -> Result<()>,
    ) -> Result<ChargedClosed> {
        if bytes.len() != BYTES {
            return Err("Partial or unbounded closed record retained".into());
        }
        let mut hash = Context::new(&SHA256);
        hash.update(DOMAIN);
        for chunk in bytes[..BYTES - 32].chunks(65536) {
            check()?;
            hash.update(chunk);
        }
        if hash.finish().as_ref() != &bytes[BYTES - 32..] {
            return Err("Closed external record SHA differs".into());
        }
        let owner = MetadataCharge::reserve(pool, BYTES * 4, check)?;
        let mut input = Input {
            bytes: &bytes[..BYTES - 32],
            at: 0,
        };
        if &input.array::<16>()? != MAGIC {
            return Err("Unknown closed external record retained".into());
        }
        let id = input.array()?;
        let root = input.array()?;
        let intent_sha = input.array()?;
        let target = input.array()?;
        let growth = input.u64()?;
        let count = input.u32()? as usize;
        let retired_count = input.array::<1>()?[0] as usize;
        if count > MAX_BINDINGS || retired_count > 3 {
            return Err("Closed external index exceeds admission".into());
        }
        let mut bindings = Vec::new();
        bindings
            .try_reserve_exact(count)
            .map_err(|_| "Closed external index allocation refused")?;
        let mut retired = Vec::new();
        retired
            .try_reserve_exact(retired_count)
            .map_err(|_| "Closed retirement index allocation refused")?;
        for _ in 0..count {
            check()?;
            let tag = input.array::<1>()?[0];
            let ordinal = input.u32()?;
            let place = match (tag, ordinal) {
                (1, 0) => Place::Target,
                (2, n) => Place::BackupOrdinal(n),
                (3, 0) => Place::CreatedBackup,
                (4, 0) => Place::Temporary,
                _ => return Err("Invalid closed location binding".into()),
            };
            bindings.push(Binding {
                place,
                fact: ExternalFact {
                    identity: input.id()?,
                    bytes: input.u64()?,
                    modified: u128::from_be_bytes(input.array()?),
                },
            });
        }
        for _ in 0..retired_count {
            retired.push(input.id()?);
        }
        if input.bytes[input.at..].iter().any(|b| *b != 0) {
            return Err("Closed external padding is not canonical".into());
        }
        let frame = Self {
            id,
            owner: root,
            intent: intent_sha,
            target,
            growth,
            bindings,
            retired,
        };
        frame.validate(original, intent)?;
        check()?;
        Ok(ChargedClosed::prepared(frame, owner))
    }
}
fn put(out: &mut impl Write, bytes: &[u8]) -> Result<()> {
    out.write_all(bytes)
        .map_err(|_| "Closed external record exceeds its fixed admission".into())
}
fn id(out: &mut impl Write, (volume, file): (u64, u64)) -> Result<()> {
    put(out, &volume.to_be_bytes())?;
    put(out, &file.to_be_bytes())
}
struct Input<'a> {
    bytes: &'a [u8],
    at: usize,
}
impl Input<'_> {
    fn array<const N: usize>(&mut self) -> Result<[u8; N]> {
        let end = self
            .at
            .checked_add(N)
            .filter(|n| *n <= self.bytes.len())
            .ok_or("Truncated closed external record")?;
        let out = self.bytes[self.at..end].try_into().unwrap();
        self.at = end;
        Ok(out)
    }
    fn u32(&mut self) -> Result<u32> {
        Ok(u32::from_be_bytes(self.array()?))
    }
    fn u64(&mut self) -> Result<u64> {
        Ok(u64::from_be_bytes(self.array()?))
    }
    fn id(&mut self) -> Result<(u64, u64)> {
        Ok((self.u64()?, self.u64()?))
    }
}

#[cfg(test)]
mod tests {
    use super::super::frame::{Expected, NamedFact};
    use super::*;
    fn original() -> IntentFrame {
        let parent = std::env::current_dir().unwrap();
        IntentFrame {
            id: [1; 16],
            owner: [2; 16],
            operation: [3; 16],
            target: parent.join("n.psb"),
            target_key: [4; 32],
            previous: None,
            parent: (1, 99),
            backups: parent.join(".peerbrush-versions"),
            backups_identity: (1, 98),
            expected: Some(Expected {
                identity: (1, 1),
                bytes: 4,
                modified: 1,
                sha256: [5; 32],
            }),
            candidate_bytes: 8,
            candidate_sha256: [6; 32],
            provenance: [7; 32],
            core: [8; 32],
            render: [9; 32],
            temporary: "native.tmp".into(),
            backup: Some("native-backup.psb".into()),
            growth: 12,
            facts: vec![
                NamedFact {
                    role: Role::Target,
                    name: "n.psb".into(),
                    fact: fact(1, 4),
                },
                NamedFact {
                    role: Role::Backup,
                    name: "old.psb".into(),
                    fact: fact(2, 3),
                },
            ],
        }
    }
    fn fact(id: u64, bytes: u64) -> ExternalFact {
        ExternalFact {
            identity: (1, id),
            bytes,
            modified: id as u128,
        }
    }
    fn closed() -> ClosedFrame {
        ClosedFrame {
            id: [1; 16],
            owner: [2; 16],
            intent: [10; 32],
            target: [4; 32],
            growth: 12,
            bindings: vec![
                Binding {
                    place: Place::BackupOrdinal(1),
                    fact: fact(2, 3),
                },
                Binding {
                    place: Place::Target,
                    fact: fact(3, 8),
                },
                Binding {
                    place: Place::CreatedBackup,
                    fact: fact(4, 4),
                },
            ],
            retired: vec![(1, 1)],
        }
    }
    #[test]
    fn fixed_length_closed_record_roundtrips_exact_locations_and_shared_accounting() {
        let pool = ResidentBudget::new(8 * 1024 * 1024).unwrap();
        let mut out = ScratchBytes::new(&pool, BYTES, BYTES).unwrap();
        let baseline = pool.used();
        let original = original();
        let frame = closed();
        let sha = frame
            .encode(&original, [10; 32], &mut out, &pool, &|| Ok(()))
            .unwrap();
        assert_eq!(sha, ring::digest::digest(&SHA256, &out).as_ref());
        let decoded = ClosedFrame::decode(&out, &original, [10; 32], &pool, &|| Ok(())).unwrap();
        assert_eq!(decoded.bindings, frame.bindings);
        assert_eq!(decoded.retired, frame.retired);
        drop(decoded);
        assert_eq!(pool.used(), baseline);
        drop(out);
        assert_eq!(pool.used(), 0);
    }
    #[test]
    fn corrupt_partial_noncanonical_wrong_intent_and_budget_refuse_without_metadata_leak() {
        let pool = ResidentBudget::new(8 * 1024 * 1024).unwrap();
        let mut out = ScratchBytes::new(&pool, BYTES, BYTES).unwrap();
        let baseline = pool.used();
        let original = original();
        closed()
            .encode(&original, [10; 32], &mut out, &pool, &|| Ok(()))
            .unwrap();
        for length in [0, 1, BYTES - 1] {
            assert!(
                ClosedFrame::decode(&out[..length], &original, [10; 32], &pool, &|| Ok(()))
                    .is_err()
            );
        }
        assert!(ClosedFrame::decode(&out, &original, [11; 32], &pool, &|| Ok(())).is_err());
        assert!(ClosedFrame::decode(
            &out,
            &original,
            [10; 32],
            &ResidentBudget::new(1).unwrap(),
            &|| Ok(())
        )
        .is_err());
        assert!(
            ClosedFrame::decode(&out, &original, [10; 32], &pool, &|| Err("cancel".into()))
                .is_err()
        );
        out[BYTES - 64] = 1;
        let mut hash = Context::new(&SHA256);
        hash.update(DOMAIN);
        hash.update(&out[..BYTES - 32]);
        out[BYTES - 32..].copy_from_slice(hash.finish().as_ref());
        assert!(ClosedFrame::decode(&out, &original, [10; 32], &pool, &|| Ok(())).is_err());
        assert_eq!(pool.used(), baseline);
    }
    #[test]
    fn missing_backup_alias_partial_temp_and_unrelated_retirement_cannot_form_closed_inventory() {
        let original = original();
        let mut f = closed();
        f.bindings.remove(0);
        assert!(f.validate(&original, [10; 32]).is_err());
        let mut f = closed();
        f.bindings[2].fact.identity = f.bindings[1].fact.identity;
        assert!(f.validate(&original, [10; 32]).is_err());
        let mut f = closed();
        f.bindings.push(Binding {
            place: Place::Temporary,
            fact: fact(5, 1),
        });
        assert!(f.validate(&original, [10; 32]).is_err());
        let mut f = closed();
        f.retired.push((1, 2));
        assert!(f.validate(&original, [10; 32]).is_err());
        let mut f = closed();
        f.retired = vec![(1, 1), (1, 1)];
        assert!(f.validate(&original, [10; 32]).is_err());
        let mut f = closed();
        f.bindings[1].fact = ExternalFact {
            modified: 99,
            ..fact(1, 4)
        };
        f.bindings.sort_unstable_by_key(|b| b.fact.identity);
        f.retired.clear();
        assert!(f.validate(&original, [10; 32]).is_err());
    }
}
