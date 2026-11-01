//! Strict quota-journal transport. Decoded paths/facts are observation hints,
//! never filesystem mutation, source adoption or resumed-operation authority.
use crate::{
    large_document::{ExternalFact, ResidentBudget},
    psd::memory36::{Admitted, MetadataCharge},
};
use ring::digest::{Context, SHA256};
use std::{
    ffi::{OsStr, OsString},
    io::{self, Write},
    path::{Component, Path, PathBuf},
    sync::Arc,
};

pub(super) const MAX_BYTES: usize = 16 * 1024 * 1024;
pub(super) const MAX_FACTS: usize = 4100;
const PATH_BYTES: usize = 64 * 1024;
const NAME_BYTES: usize = 4096;
const MAGIC: &[u8; 16] = b"PBEXTERNAL01\0\0\0\0";
const DOMAIN: &[u8] = b"PeerBrushExternalIntentFrame1\0";
type Result<T> = std::result::Result<T, String>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Expected {
    pub identity: (u64, u64),
    pub bytes: u64,
    pub modified: u128,
    pub sha256: [u8; 32],
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Role {
    Target,
    Backup,
    Temporary,
}
#[derive(Debug)]
pub(super) struct NamedFact {
    pub role: Role,
    pub name: OsString,
    pub fact: ExternalFact,
}
#[derive(Debug)]
pub(super) struct IntentFrame {
    pub id: [u8; 16],
    pub owner: [u8; 16],
    pub operation: [u8; 16],
    pub target: PathBuf,
    pub target_key: [u8; 32],
    pub parent: (u64, u64),
    pub backups: PathBuf,
    pub backups_identity: (u64, u64),
    pub expected: Option<Expected>,
    pub candidate_bytes: u64,
    pub candidate_sha256: [u8; 32],
    pub provenance: [u8; 32],
    pub core: [u8; 32],
    pub render: [u8; 32],
    pub temporary: OsString,
    pub backup: Option<OsString>,
    pub growth: u64,
    pub facts: Vec<NamedFact>,
}
impl IntentFrame {
    pub(super) fn validate(&self) -> Result<()> {
        if self.id == [0; 16]
            || self.owner == [0; 16]
            || self.operation == [0; 16]
            || self.target_key == [0; 32]
            || self.candidate_bytes == 0
            || self.candidate_bytes > 64 * 1024 * 1024 * 1024
            || self.facts.len() > MAX_FACTS
            || !absolute(&self.target)
            || !absolute(&self.backups)
            || self.target.parent().is_none()
            || self.backups.parent() != self.target.parent()
            || self.backups.file_name() != Some(OsStr::new(".peerbrush-versions"))
            || !name(&self.temporary)
            || self.backup.as_deref().is_some_and(|n| !name(n))
            || self.backup.is_some() != self.expected.is_some()
            || self.growth
                != self
                    .candidate_bytes
                    .checked_add(self.expected.map_or(0, |e| e.bytes))
                    .ok_or("External growth overflow")?
        {
            return Err("External intent fields exceed their strict admission".into());
        }
        let mut previous = None;
        let mut targets = 0;
        for entry in &self.facts {
            if !name(&entry.name)
                || previous.is_some_and(|id| id >= entry.fact.identity)
                || entry.role == Role::Temporary
                || entry.role == Role::Target
                    && (Some(entry.name.as_os_str()) != self.target.file_name()
                        || self.expected.is_none_or(|e| {
                            e.identity != entry.fact.identity
                                || e.bytes != entry.fact.bytes
                                || e.modified != entry.fact.modified
                        }))
            {
                return Err(
                    "External intent facts are ambiguous or outside their captured names".into(),
                );
            }
            previous = Some(entry.fact.identity);
            targets += usize::from(entry.role == Role::Target);
        }
        if targets != usize::from(self.expected.is_some()) {
            return Err("External current-file facts are incomplete".into());
        }
        Ok(())
    }
    pub(super) fn encoded_len(&self) -> Result<usize> {
        self.validate()?;
        let mut count = Counter(0);
        self.body(&mut count)?;
        count
            .0
            .checked_add(32)
            .filter(|n| *n <= MAX_BYTES)
            .ok_or_else(|| "External intent frame exceeds its bound".into())
    }
    pub(super) fn encode(
        &self,
        pool: &ResidentBudget,
        check: &dyn Fn() -> Result<()>,
    ) -> Result<Admitted<Vec<u8>>> {
        let length = self.encoded_len()?;
        let owner = MetadataCharge::reserve(
            pool,
            length
                .checked_mul(2)
                .ok_or("External frame allocation overflow")?,
            check,
        )?;
        let mut bytes = Vec::new();
        bytes
            .try_reserve_exact(length)
            .map_err(|_| "External frame allocation refused")?;
        self.body(&mut bytes)?;
        let mut hash = Context::new(&SHA256);
        hash.update(DOMAIN);
        for chunk in bytes.chunks(65536) {
            check()?;
            hash.update(chunk);
        }
        bytes.extend(hash.finish().as_ref());
        if bytes.len() != length || bytes.capacity() > owner.bytes() {
            return Err("External encoded frame footprint changed".into());
        }
        check()?;
        Ok(Admitted::from_parts(Arc::new(bytes), owner))
    }
    fn body(&self, out: &mut impl Write) -> Result<()> {
        put(out, MAGIC)?;
        put(out, &[platform()])?;
        put(out, &self.id)?;
        put(out, &self.owner)?;
        put(out, &self.operation)?;
        put(out, &self.target_key)?;
        identity(out, self.parent)?;
        identity(out, self.backups_identity)?;
        os(out, self.target.as_os_str(), PATH_BYTES)?;
        os(out, self.backups.as_os_str(), PATH_BYTES)?;
        put(out, &[u8::from(self.expected.is_some())])?;
        if let Some(e) = self.expected {
            identity(out, e.identity)?;
            put(out, &e.bytes.to_be_bytes())?;
            put(out, &e.modified.to_be_bytes())?;
            put(out, &e.sha256)?;
        }
        put(out, &self.candidate_bytes.to_be_bytes())?;
        put(out, &self.candidate_sha256)?;
        put(out, &self.provenance)?;
        put(out, &self.core)?;
        put(out, &self.render)?;
        os(out, &self.temporary, NAME_BYTES)?;
        put(out, &[u8::from(self.backup.is_some())])?;
        if let Some(backup) = &self.backup {
            os(out, backup, NAME_BYTES)?;
        }
        put(out, &self.growth.to_be_bytes())?;
        put(out, &(self.facts.len() as u32).to_be_bytes())?;
        for entry in &self.facts {
            put(
                out,
                &[match entry.role {
                    Role::Target => 1,
                    Role::Backup => 2,
                    Role::Temporary => 3,
                }],
            )?;
            os(out, &entry.name, NAME_BYTES)?;
            identity(out, entry.fact.identity)?;
            put(out, &entry.fact.bytes.to_be_bytes())?;
            put(out, &entry.fact.modified.to_be_bytes())?;
        }
        Ok(())
    }
    pub(super) fn decode(
        bytes: &[u8],
        pool: &ResidentBudget,
        check: &dyn Fn() -> Result<()>,
    ) -> Result<Admitted<Self>> {
        if bytes.len() < 32 || bytes.len() > MAX_BYTES {
            return Err("External journal frame length differs".into());
        }
        let body = bytes.len() - 32;
        let mut hash = Context::new(&SHA256);
        hash.update(DOMAIN);
        for chunk in bytes[..body].chunks(65536) {
            check()?;
            hash.update(chunk);
        }
        if hash.finish().as_ref() != &bytes[body..] {
            return Err("External journal SHA differs; debt cannot be inferred".into());
        }
        let owner = MetadataCharge::reserve(
            pool,
            bytes
                .len()
                .checked_mul(4)
                .and_then(|n| n.checked_add(1024 * 1024))
                .ok_or("External decode footprint overflow")?,
            check,
        )?;
        let mut input = Input {
            bytes: &bytes[..body],
            at: 0,
        };
        if input.take(16)? != MAGIC || input.u8()? != platform() {
            return Err("Unknown external journal platform/version retained".into());
        }
        let id = input.array()?;
        let root = input.array()?;
        let operation = input.array()?;
        let target_key = input.array()?;
        let parent = input.identity()?;
        let backups_identity = input.identity()?;
        let target = PathBuf::from(input.os(PATH_BYTES)?);
        let backups = PathBuf::from(input.os(PATH_BYTES)?);
        let expected = match input.u8()? {
            0 => None,
            1 => Some(Expected {
                identity: input.identity()?,
                bytes: input.u64()?,
                modified: input.u128()?,
                sha256: input.array()?,
            }),
            _ => return Err("Invalid external expected-version flag".into()),
        };
        let candidate_bytes = input.u64()?;
        let candidate_sha256 = input.array()?;
        let provenance = input.array()?;
        let core = input.array()?;
        let render = input.array()?;
        let temporary = input.os(NAME_BYTES)?;
        let backup = match input.u8()? {
            0 => None,
            1 => Some(input.os(NAME_BYTES)?),
            _ => return Err("Invalid planned backup flag".into()),
        };
        let growth = input.u64()?;
        let count = input.u32()? as usize;
        if count > MAX_FACTS {
            return Err("External fact count exceeds admission".into());
        }
        let mut facts = Vec::new();
        facts
            .try_reserve_exact(count)
            .map_err(|_| "External fact allocation refused")?;
        for _ in 0..count {
            check()?;
            let role = match input.u8()? {
                1 => Role::Target,
                2 => Role::Backup,
                _ => return Err("Invalid external intent fact role".into()),
            };
            let name = input.os(NAME_BYTES)?;
            facts.push(NamedFact {
                role,
                name,
                fact: ExternalFact {
                    identity: input.identity()?,
                    bytes: input.u64()?,
                    modified: input.u128()?,
                },
            });
        }
        if input.at != body {
            return Err("External journal has trailing fields".into());
        }
        let frame = Self {
            id,
            owner: root,
            operation,
            target,
            target_key,
            parent,
            backups,
            backups_identity,
            expected,
            candidate_bytes,
            candidate_sha256,
            provenance,
            core,
            render,
            temporary,
            backup,
            growth,
            facts,
        };
        frame.validate()?;
        check()?;
        Ok(Admitted::from_parts(Arc::new(frame), owner))
    }
}
fn absolute(path: &Path) -> bool {
    path.is_absolute()
        && !path
            .components()
            .any(|c| matches!(c, Component::ParentDir | Component::CurDir))
}
fn name(value: &OsStr) -> bool {
    let mut components = Path::new(value).components();
    matches!(components.next(), Some(Component::Normal(_))) && components.next().is_none()
}
fn put(out: &mut impl Write, bytes: &[u8]) -> Result<()> {
    out.write_all(bytes).map_err(|e| e.to_string())
}
fn identity(out: &mut impl Write, id: (u64, u64)) -> Result<()> {
    put(out, &id.0.to_be_bytes())?;
    put(out, &id.1.to_be_bytes())
}
#[cfg(windows)]
fn platform() -> u8 {
    2
}
#[cfg(unix)]
fn platform() -> u8 {
    1
}
fn os(out: &mut impl Write, value: &OsStr, maximum: usize) -> Result<()> {
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        let count = value
            .encode_wide()
            .count()
            .checked_mul(2)
            .filter(|n| *n <= maximum)
            .ok_or("External OS field exceeds bound")?;
        put(out, &(count as u32).to_be_bytes())?;
        for unit in value.encode_wide() {
            if unit == 0 {
                return Err("External name contains NUL".into());
            }
            put(out, &unit.to_le_bytes())?;
        }
    }
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        let bytes = value.as_bytes();
        if bytes.len() > maximum || bytes.contains(&0) {
            return Err("External OS field exceeds bound or contains NUL".into());
        }
        put(out, &(bytes.len() as u32).to_be_bytes())?;
        put(out, bytes)?;
    }
    Ok(())
}
struct Counter(usize);
impl Write for Counter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0 = self
            .0
            .checked_add(bytes.len())
            .filter(|n| *n <= MAX_BYTES)
            .ok_or_else(|| io::Error::other("External frame exceeds bound"))?;
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
struct Input<'a> {
    bytes: &'a [u8],
    at: usize,
}
impl Input<'_> {
    fn take(&mut self, n: usize) -> Result<&[u8]> {
        let end = self
            .at
            .checked_add(n)
            .filter(|n| *n <= self.bytes.len())
            .ok_or("Partial external journal retained")?;
        let bytes = &self.bytes[self.at..end];
        self.at = end;
        Ok(bytes)
    }
    fn array<const N: usize>(&mut self) -> Result<[u8; N]> {
        Ok(self.take(N)?.try_into().unwrap())
    }
    fn u8(&mut self) -> Result<u8> {
        Ok(self.array::<1>()?[0])
    }
    fn u32(&mut self) -> Result<u32> {
        Ok(u32::from_be_bytes(self.array()?))
    }
    fn u64(&mut self) -> Result<u64> {
        Ok(u64::from_be_bytes(self.array()?))
    }
    fn u128(&mut self) -> Result<u128> {
        Ok(u128::from_be_bytes(self.array()?))
    }
    fn identity(&mut self) -> Result<(u64, u64)> {
        Ok((self.u64()?, self.u64()?))
    }
    fn os(&mut self, maximum: usize) -> Result<OsString> {
        let count = self.u32()? as usize;
        if count > maximum {
            return Err("External OS field exceeds bound".into());
        }
        let bytes = self.take(count)?;
        #[cfg(windows)]
        {
            use std::os::windows::ffi::OsStringExt;
            if bytes.len() % 2 != 0 {
                return Err("Invalid native UTF16 field".into());
            }
            let mut units = Vec::new();
            units
                .try_reserve_exact(bytes.len() / 2)
                .map_err(|_| "External UTF16 allocation refused")?;
            units.extend(
                bytes
                    .chunks_exact(2)
                    .map(|p| u16::from_le_bytes([p[0], p[1]])),
            );
            if units.contains(&0) {
                return Err("External OS field contains NUL".into());
            }
            Ok(OsString::from_wide(&units))
        }
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStringExt;
            if bytes.contains(&0) {
                return Err("External OS field contains NUL".into());
            }
            let mut value = Vec::new();
            value
                .try_reserve_exact(bytes.len())
                .map_err(|_| "External OS allocation refused")?;
            value.extend_from_slice(bytes);
            Ok(OsString::from_vec(value))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn frame() -> IntentFrame {
        let parent = std::env::current_dir().unwrap();
        IntentFrame {
            id: [1; 16],
            owner: [2; 16],
            operation: [3; 16],
            target: parent.join("native-\u{05d0}.psb"),
            target_key: [4; 32],
            parent: (1, 2),
            backups: parent.join(".peerbrush-versions"),
            backups_identity: (1, 3),
            expected: Some(Expected {
                identity: (1, 4),
                bytes: 8,
                modified: 17,
                sha256: [5; 32],
            }),
            candidate_bytes: 16,
            candidate_sha256: [6; 32],
            provenance: [7; 32],
            core: [8; 32],
            render: [9; 32],
            temporary: ".peerbrush-native-test.tmp".into(),
            backup: Some("native-test.psb".into()),
            growth: 24,
            facts: vec![
                NamedFact {
                    role: Role::Target,
                    name: "native-\u{05d0}.psb".into(),
                    fact: ExternalFact {
                        identity: (1, 4),
                        bytes: 8,
                        modified: 17,
                    },
                },
                NamedFact {
                    role: Role::Backup,
                    name: "prior-\u{1f58c}.psb".into(),
                    fact: ExternalFact {
                        identity: (1, 5),
                        bytes: 9,
                        modified: 19,
                    },
                },
            ],
        }
    }
    #[test]
    fn quota_transport_roundtrips_exact_native_names_and_releases_all_metadata() {
        let pool = ResidentBudget::new(8 * 1024 * 1024).unwrap();
        let f = frame();
        let encoded = f.encode(&pool, &|| Ok(())).unwrap();
        let original = pool.used();
        let decoded = IntentFrame::decode(&encoded, &pool, &|| Ok(())).unwrap();
        assert_eq!(decoded.target, f.target);
        assert_eq!(decoded.backups, f.backups);
        assert_eq!(decoded.expected, f.expected);
        assert_eq!(decoded.facts[1].name, f.facts[1].name);
        assert_eq!(decoded.facts[1].fact, f.facts[1].fact);
        assert_eq!(decoded.temporary, f.temporary);
        let reencoded = decoded.encode(&pool, &|| Ok(())).unwrap();
        assert_eq!(&**encoded, &**reencoded);
        drop(reencoded);
        drop(decoded);
        assert_eq!(pool.used(), original);
        drop(encoded);
        assert_eq!(pool.used(), 0);
    }
    #[test]
    fn partial_changed_trailing_unknown_and_cancelled_frames_never_infer_debt() {
        let pool = ResidentBudget::new(8 * 1024 * 1024).unwrap();
        let encoded = frame().encode(&pool, &|| Ok(())).unwrap();
        let baseline = pool.used();
        for at in 0..encoded.len() {
            assert!(IntentFrame::decode(&encoded[..at], &pool, &|| Ok(())).is_err());
            assert_eq!(pool.used(), baseline);
        }
        for at in [0, 16, 48, encoded.len() - 1] {
            let mut bytes = encoded.to_vec();
            bytes[at] ^= 1;
            assert!(IntentFrame::decode(&bytes, &pool, &|| Ok(())).is_err());
        }
        // A complete new checksum still cannot admit a different schema/platform.
        let mut bytes = encoded.to_vec();
        bytes[16] = 255;
        let body = bytes.len() - 32;
        let mut hash = Context::new(&SHA256);
        hash.update(DOMAIN);
        hash.update(&bytes[..body]);
        bytes[body..].copy_from_slice(hash.finish().as_ref());
        assert!(IntentFrame::decode(&bytes, &pool, &|| Ok(())).is_err());
        let mut trailing = encoded.to_vec();
        trailing.push(0);
        assert!(IntentFrame::decode(&trailing, &pool, &|| Ok(())).is_err());
        assert!(IntentFrame::decode(&encoded, &pool, &|| Err("cancelled".into())).is_err());
        assert!(
            IntentFrame::decode(&encoded, &ResidentBudget::new(1).unwrap(), &|| Ok(())).is_err()
        );
        assert_eq!(pool.used(), baseline);
        drop(encoded);
        assert_eq!(pool.used(), 0);
    }
    #[test]
    fn duplicate_physical_facts_incomplete_current_and_unbounded_names_refuse_before_encoding() {
        let pool = ResidentBudget::new(8 * 1024 * 1024).unwrap();
        let mut f = frame();
        f.facts[1].fact.identity = f.facts[0].fact.identity;
        assert!(f.encode(&pool, &|| Ok(())).is_err());
        assert_eq!(pool.used(), 0);
        let mut f = frame();
        f.facts.remove(0);
        assert!(f.encoded_len().is_err());
        let mut f = frame();
        f.temporary = "../outside".into();
        assert!(f.encoded_len().is_err());
        let mut f = frame();
        f.backup = Some("z".repeat(NAME_BYTES + 1).into());
        assert!(f.encoded_len().is_err());
        let mut f = frame();
        f.growth -= 1;
        assert!(f.encoded_len().is_err());
        let mut f = frame();
        f.backups = f.backups.join("nested");
        assert!(f.encoded_len().is_err());
        assert_eq!(pool.used(), 0);
    }
}
